//! Integration tests for par block codegen (Phase 7).
//!
//! These tests verify:
//! - IR-level: par blocks lower to a `karac_par_run` call with the correct
//!   number of branch function pointers.
//! - End-to-end: a compiled par program statically links the runtime, spawns
//!   real threads, and produces output from every branch.
//!
//! The end-to-end tests build the runtime crate on first use via
//! `cargo build -p karac-runtime --release`. If that build fails (e.g., no
//! Cargo available in the test environment) the tests soft-skip by returning
//! early, matching the pattern in tests/codegen.rs.

mod common;

#[cfg(feature = "llvm")]
mod par_codegen_tests {
    use karac::codegen::compile_to_ir;
    use std::path::PathBuf;
    use std::sync::Once;

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    /// Build the runtime static library once per test process and return its
    /// path. Returns None if the build fails — callers soft-skip.
    #[allow(static_mut_refs)]
    fn runtime_path() -> Option<PathBuf> {
        RUNTIME_BUILT.call_once(|| {
            let output = std::process::Command::new("cargo")
                .args(["build", "-p", "karac-runtime", "--release"])
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("target/release/libkarac_runtime.a");
                    if p.exists() {
                        unsafe {
                            RUNTIME_PATH = Some(p);
                        }
                    }
                }
            }
        });
        unsafe { RUNTIME_PATH.clone() }
    }

    fn ir_for(src: &str) -> String {
        let parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        compile_to_ir(&parsed.program, None, None).expect("codegen failed")
    }

    /// Like `ir_for` but runs the full analysis pipeline first so the
    /// `Program.callee_effectful` side-table is populated. Required for the
    /// par-branch cancel-check narrowing — without effect-check info every
    /// callee is unknown and the check fires conservatively.
    fn ir_for_with_pipeline(src: &str) -> String {
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
        // Mirror `Pipeline::effectcheck`: a callee is "effectful" iff its
        // inferred or declared set contains reads/writes/sends/receives.
        use karac::effectchecker::DeclaredEffects;
        fn set_eff(s: &karac::effectchecker::EffectSet) -> bool {
            s.effects.iter().any(|t| {
                matches!(
                    t.effect.verb,
                    karac::ast::EffectVerbKind::Reads
                        | karac::ast::EffectVerbKind::Writes
                        | karac::ast::EffectVerbKind::Sends
                        | karac::ast::EffectVerbKind::Receives
                )
            })
        }
        let mut table = std::collections::HashMap::new();
        for (name, set) in &effects.inferred_effects {
            table.insert(name.clone(), set_eff(set));
        }
        for (name, decl) in &effects.declared_effects {
            let eff = match decl {
                DeclaredEffects::Explicit(s) => set_eff(s),
                DeclaredEffects::Polymorphic | DeclaredEffects::PolymorphicWithFixed(_) => true,
                DeclaredEffects::None => false,
            };
            table
                .entry(name.clone())
                .and_modify(|v| *v = *v || eff)
                .or_insert(eff);
        }
        parsed.program.callee_effectful = table;
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen failed")
    }

    #[test]
    fn collect_all_vec_lowers_to_par_run_gather() {
        // Phase 6 slice 1b — `collect_all_vec(fs)` lowers to a parallel
        // gather: a shared `__collect_all_vec_branch` trampoline invoked
        // per element via `karac_par_run`, each writing a Result into a
        // malloc'd slot that becomes the output Vec's buffer. (Slice 1a's
        // hard-error gate is gone; this asserts the lowering is present.)
        // The closures here are the canonical fan-out shape — a captured
        // arg + a named call returning Result (`|| work(a)`).
        let src = "fn work(n: i64) -> Result[i64, String] {\n\
                       if n > 0 { Result.Ok(n) } else { Result.Err(\"neg\") }\n\
                   }\n\
                   fn main() {\n\
                       let a: i64 = 1;\n\
                       let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| work(a)];\n\
                       let r: Vec[Result[i64, String]] = collect_all_vec(fs);\n\
                       let _n: i64 = r.len();\n\
                   }";
        let mut parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        let ir = compile_to_ir(&parsed.program, Some(&ownership), None)
            .expect("collect_all_vec must lower under karac build in slice 1b");
        assert!(
            ir.contains("__collect_all_vec_branch"),
            "expected the shared gather trampoline in the IR"
        );
        assert!(
            ir.contains("karac_par_run"),
            "expected the karac_par_run dispatch in the IR"
        );
    }

    #[test]
    fn closure_value_inline_result_and_fstring_codegen_ok() {
        // closure-value-codegen-fixes — a closure VALUE whose body
        // inline-constructs an enum variant (`|| Result.Ok(x)`) and one
        // that builds an f-string (`|| Result.Err(f"…")`) must lower to
        // valid IR. Pre-fix two distinct LLVM-verifier failures (each its
        // own bug surfaced by collect_all_vec): (1) closure return-type
        // inference returned the payload type, so the closure fn `ret`'d a
        // `{i64×6}` Result where the signature said `i64` ("return type
        // does not match operand type of return inst"); (2) the f-string
        // accumulator's cleanup leaked into the OUTER fn's frame, emitting
        // a GEP into a closure-fn alloca ("Instruction does not dominate
        // all uses"). `compile_to_ir` runs the module verifier, so a
        // regression on either re-surfaces here as an `Err`.
        let src = "fn main() {\n\
                       let base: i64 = 100;\n\
                       let ok: Fn() -> Result[i64, String] = || Result.Ok(base + 1);\n\
                       let err: Fn() -> Result[i64, String] = || Result.Err(f\"bad{base}\");\n\
                       let _a: Result[i64, String] = ok();\n\
                       let _b: Result[i64, String] = err();\n\
                   }";
        let mut parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        let ir = compile_to_ir(&parsed.program, Some(&ownership), None)
            .expect("inline-Result + f-string closure VALUES must lower to verifier-clean IR");
        // The closure fns return the 6-word type-erased Result struct, not
        // the i64 payload fallback.
        assert!(
            ir.contains("{ i64, i64, i64, i64, i64, i64 }"),
            "expected the Result struct type in the closure IR"
        );
    }

    #[test]
    fn collect_all_tuple_lowers_to_par_run_gather() {
        // Phase 6 — `collect_all(|| a, || b, || c)` lowers to the same
        // `karac_par_run` + `__collect_all_vec_branch` trampoline gather as
        // `collect_all_vec`, but static-N with a tuple result. The
        // heterogeneous tuple is a struct of three type-erased `{i64×6}`
        // Result structs.
        let src = "fn fa(n: i64) -> Result[i64, String] { Result.Ok(n) }\n\
                   fn fb(s: String) -> Result[String, i64] { Result.Err(1) }\n\
                   fn main() {\n\
                       let a: i64 = 1;\n\
                       let t: (Result[i64, String], Result[String, i64]) =\n\
                           collect_all(|| fa(a), || fb(\"x\"));\n\
                       let _0: Result[i64, String] = t.0;\n\
                   }";
        let mut parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        let ir = compile_to_ir(&parsed.program, Some(&ownership), None)
            .expect("collect_all must lower under karac build");
        assert!(
            ir.contains("__collect_all_vec_branch"),
            "expected the shared gather trampoline in the IR"
        );
        assert!(
            ir.contains("karac_par_run"),
            "expected the karac_par_run dispatch in the IR"
        );
    }

    /// Compile, link with the runtime, and run the program. Returns stdout
    /// on success, None if link/exec fails (legitimate soft-skip when the
    /// runtime archive is missing). Parse and codegen failures panic — those
    /// are programming bugs, not environment issues.
    ///
    /// Runs the full analysis pipeline (resolve / typecheck / lower /
    /// effectcheck / concurrency_analyze) and threads the concurrency
    /// analysis into codegen, so reduction recognition (slice 1) actually
    /// fires and the lowering (slice 3b onward) is exercised end-to-end.
    /// Without this pipeline the par_reduce path would never be reached,
    /// and reduction E2E tests would silently validate only the sequential
    /// fallback's output (which happens to match by design, masking any
    /// real lowering regression).
    fn run_program(src: &str) -> Option<String> {
        use karac::codegen::{compile_to_object, link_executable};
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let rt = runtime_path()?;
        std::env::set_var("KARAC_RUNTIME", &rt);

        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            let mut msg = String::from("test source failed to parse:\n");
            for e in &parsed.errors {
                msg.push_str(&format!("  {:?}\n", e));
            }
            panic!("{}", msg);
        }
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        // Thread type info so method-call network fan-out (A2b-2 Phase 2 Slice 2)
        // is enabled end-to-end, as the real CLI pipeline does.
        let analysis = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let obj_path = format!("/tmp/karac_par_e2e_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_par_e2e_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object(
            &parsed.program,
            &obj_path,
            Some(&ownership),
            Some(&analysis),
        ) {
            panic!("codegen failed for test program: {}", e);
        }
        link_executable(&obj_path, &exe_path).ok()?;

        let output = super::common::output_with_hang_watchdog(
            std::process::Command::new(&exe_path),
            std::time::Duration::from_secs(60),
        )?;

        let _ = std::fs::remove_file(&obj_path);
        let _ = std::fs::remove_file(&exe_path);

        Some(String::from_utf8_lossy(&output.stdout).to_string())
    }

    // ── Atomic op arity — run/build agreement (B-2026-06-30-5) ─────

    /// E2E: the explicit-ordering atomic form (`fetch_add(v, ord)` through a
    /// `ref` param + `load(ord)` on the owned binding) compiles and runs,
    /// printing `3`. This is the codegen half of the run/build agreement the
    /// arity bug broke — its interpreter twin is
    /// `test_atomic_fetch_add_load_explicit_agrees_with_codegen` in
    /// tests/interpreter.rs, and both assert `3`.
    #[test]
    fn test_e2e_atomic_fetch_add_load_explicit() {
        let out = run_program(
            r#"
par struct Counter { count: Atomic[i64] }
fn bump(c: ref Counter) { let _ = c.count.fetch_add(1, MemoryOrdering.Relaxed); }
fn main() {
    let c = Counter { count: Atomic.new(0) };
    par { bump(c); bump(c); bump(c); }
    println(c.count.load(MemoryOrdering.Relaxed));
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "3",
                "explicit atomic fetch_add/load must total 3 (interpreter parity); got {out:?}"
            );
        }
    }

    /// Regression: codegen must keep REJECTING the implicit-ordering form (the
    /// `build` side of B-2026-06-30-5) rather than silently defaulting an
    /// ordering. Uses a `ref`-param receiver whose field types as `Type::Error`
    /// in the typechecker (fields.rs), so codegen — not typecheck — is the
    /// rejecting layer under test. `compile_to_object` needs no runtime
    /// archive, so this runs everywhere the llvm feature is built.
    #[test]
    fn test_atomic_implicit_ordering_rejected_by_codegen() {
        use karac::codegen::compile_to_object;
        let src = r#"
par struct Counter { count: Atomic[i64] }
fn bump(c: ref Counter) { let _ = c.count.fetch_add(1); }
fn main() {
    let c = Counter { count: Atomic.new(0) };
    bump(c);
    println(c.count.load(MemoryOrdering.Relaxed));
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "source must parse");
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        // Thread type info so method-call network fan-out (A2b-2 Phase 2 Slice 2)
        // is enabled end-to-end, as the real CLI pipeline does.
        let analysis = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));
        let obj_path = format!("/tmp/karac_atomic_reject_{}.o", std::process::id());
        let err = compile_to_object(
            &parsed.program,
            &obj_path,
            Some(&ownership),
            Some(&analysis),
        )
        .expect_err("codegen must reject the implicit-ordering fetch_add");
        let msg = err.to_string();
        assert!(
            msg.contains("Atomic.fetch_add") && msg.contains("MemoryOrdering"),
            "codegen rejection should name the op + required ordering; got: {msg}"
        );
        let _ = std::fs::remove_file(&obj_path);
    }

    // ── TaskGroup / spawn — run/build agreement (B-2026-06-30-8) ───

    /// E2E: the canonical explicit-join TaskGroup fan-out — spawn two
    /// children, join each handle, sum. Compiles and runs, printing `60`
    /// (worker(10)=20 + worker(20)=40). This is the codegen half of the
    /// run/build agreement the interpreter's missing TaskGroup rule broke
    /// (`TaskGroup.new` hit the "not wired in the tree-walk interpreter"
    /// internal error); its interpreter twin is
    /// `test_taskgroup_spawn_join_agrees_with_codegen` in
    /// tests/interpreter.rs, and both assert `60`.
    #[test]
    fn test_e2e_taskgroup_spawn_join() {
        let out = run_program(
            r#"
fn worker(n: i64) -> i64 { n * 2 }
fn main() {
    let mut tg = TaskGroup.new();
    let h1: TaskHandle[i64] = tg.spawn(|| worker(10));
    let h2: TaskHandle[i64] = tg.spawn(|| worker(20));
    let r1: i64 = h1.join();
    let r2: i64 = h2.join();
    println(r1 + r2);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "60",
                "TaskGroup spawn/join fan-out must total 60 (interpreter parity); got {out:?}"
            );
        }
    }

    /// E2E: free `spawn(closure)` + `handle.join()` — the unscoped sibling of
    /// `tg.spawn`. Prints `42`. Interpreter twin is
    /// `test_free_spawn_join_agrees_with_codegen` in tests/interpreter.rs;
    /// before B-2026-06-30-8 `karac run` panicked on the unresolved `spawn`
    /// identifier while `karac build` compiled it fine.
    #[test]
    fn test_e2e_free_spawn_join() {
        let out = run_program(
            r#"
fn add(a: i64, b: i64) -> i64 { a + b }
fn main() {
    let h: TaskHandle[i64] = spawn(|| add(40, 2));
    let r: i64 = h.join();
    println(r);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "42",
                "free spawn/join must print 42 (interpreter parity); got {out:?}"
            );
        }
    }

    // ── IR-level tests ────────────────────────────────────────────

    #[test]
    fn test_ir_par_block_emits_runtime_call() {
        let ir = ir_for(
            r#"
fn main() {
    par {
        println(100);
        println(200);
    }
}
"#,
        );
        assert!(
            ir.contains("declare void @karac_par_run"),
            "IR should declare karac_par_run; got:\n{ir}"
        );
        assert!(
            ir.contains("call void @karac_par_run"),
            "IR should call karac_par_run; got:\n{ir}"
        );
        assert!(
            ir.contains("__par_branch_0_0"),
            "IR should define first branch fn"
        );
        assert!(
            ir.contains("__par_branch_0_1"),
            "IR should define second branch fn"
        );
    }

    /// Debugger Contract slice 4: `karac_par_run` takes a `spawn_site_id`
    /// argument (the same `par_id` minted via slice 3's
    /// `record_spawn_site`). With two par blocks in the program, the call
    /// sites must pass `i32 0` and `i32 1` respectively — pinning the
    /// codegen-side argument-passing change against future regression.
    /// The runtime uses this ID to populate `KaracFrame::spawn_site_id`
    /// for slice 5's enumeration surface.
    #[test]
    fn test_emit_par_run_passes_spawn_site_id() {
        let ir = ir_for(
            r#"
fn main() {
    par {
        println(1);
        println(2);
    }
    par {
        println(3);
        println(4);
    }
}
"#,
        );
        // The extern declaration's signature: branches ptr, count i64,
        // spawn-site id i32, and the parent-cancel ptr (phase-6 line 475,
        // null at top level).
        assert!(
            ir.contains("declare void @karac_par_run(ptr, i64, i32, ptr)"),
            "extern decl should be (ptr, i64, i32, ptr); got:\n{ir}"
        );
        // Two call sites — one with spawn_site_id 0, one with 1.
        // Inkwell emits the actual call as
        // `call void @karac_par_run(ptr ..., i64 ..., i32 0, ptr null)`
        // (the spawn-site id is now followed by the parent-cancel ptr).
        let calls: Vec<&str> = ir
            .lines()
            .filter(|l| l.contains("call void @karac_par_run"))
            .collect();
        assert_eq!(
            calls.len(),
            2,
            "expected exactly two karac_par_run calls; got {}: {:?}",
            calls.len(),
            calls
        );
        let mut seen_zero = false;
        let mut seen_one = false;
        for c in &calls {
            if c.contains("i32 0, ptr") {
                seen_zero = true;
            }
            if c.contains("i32 1, ptr") {
                seen_one = true;
            }
        }
        assert!(
            seen_zero,
            "expected one call with spawn_site_id `i32 0`; calls:\n{:?}",
            calls
        );
        assert!(
            seen_one,
            "expected one call with spawn_site_id `i32 1`; calls:\n{:?}",
            calls
        );
    }

    /// Phase-6 line 475 — nested cancellation cascade wiring. A `par` block
    /// nested inside an outer branch must pass the *enclosing* branch's
    /// cancel flag as `karac_par_run`'s `parent_cancel` arg, so the runtime
    /// join can cascade an outer cancel inward. The top-level `par` passes
    /// `ptr null` (no enclosing region). We assert both: a null parent at the
    /// top level, and a non-null (`ptr %...`) parent at the nested call.
    #[test]
    fn test_nested_par_passes_enclosing_branch_cancel_as_parent() {
        let ir = ir_for(
            r#"
fn main() {
    par {
        par {
            println(1);
            println(2);
        }
        println(3);
    }
}
"#,
        );
        let calls: Vec<&str> = ir
            .lines()
            .map(|l| l.trim())
            .filter(|l| l.contains("call void @karac_par_run"))
            .collect();
        assert_eq!(
            calls.len(),
            2,
            "expected one outer + one nested karac_par_run; got {}: {:?}",
            calls.len(),
            calls
        );
        // Top-level call: parent_cancel is null.
        assert!(
            calls.iter().any(|c| c.ends_with("ptr null)")),
            "the top-level par must pass `ptr null` as parent_cancel; calls:\n{:?}",
            calls
        );
        // Nested call: parent_cancel is the enclosing branch's cancel ptr
        // (an SSA value `ptr %...`), NOT null.
        assert!(
            calls.iter().any(|c| c.ends_with("ptr %cancel)")
                || (c.contains("ptr %") && !c.ends_with("ptr null)"))),
            "the nested par must pass the enclosing branch's cancel ptr (non-null) as \
             parent_cancel; calls:\n{:?}",
            calls
        );
    }

    #[test]
    fn test_ir_par_single_stmt_no_runtime_call() {
        // Par with one statement is optimized to sequential — no runtime call.
        let ir = ir_for(
            r#"
fn main() {
    par {
        println(42);
    }
}
"#,
        );
        assert!(
            !ir.contains("call void @karac_par_run"),
            "single-stmt par should not call runtime; got:\n{ir}"
        );
    }

    #[test]
    fn test_ir_par_empty_block_no_runtime_call() {
        let ir = ir_for(
            r#"
fn main() {
    par {
    }
}
"#,
        );
        assert!(
            !ir.contains("call void @karac_par_run"),
            "empty par should not call runtime; got:\n{ir}"
        );
    }

    // ── Mid-branch cooperative cancellation ───────────────────────────────

    /// Count mid-branch cancel checks across every par-branch function in
    /// the IR. Each top-level statement inside `par { }` lowers to its own
    /// branch fn (e.g. `__par_branch_0_0`, `__par_branch_0_1`), so per-call
    /// narrowing is observed by aggregating across all of them. We key on
    /// `call.cancel.flag = load` — the unique atomic-flag load instruction
    /// that opens each mid-branch check (the entry-time check uses a
    /// different `%cancel` SSA name).
    fn count_branch_cancel_checks(ir: &str) -> usize {
        let mut total = 0;
        let mut cursor = 0;
        while let Some(off) = ir[cursor..].find("define void @__par_branch_") {
            let start = cursor + off;
            let end = ir[start + 1..]
                .find("define ")
                .map(|i| start + 1 + i)
                .unwrap_or(ir.len());
            total += ir[start..end].matches("call.cancel.flag = load").count();
            cursor = end;
        }
        total
    }

    #[test]
    fn test_ir_par_branch_emits_cancel_check_per_effectful_call() {
        // Each call to an effectful helper inside a par branch should emit a
        // mid-branch cancel check (load-and-branch on the runtime atomic).
        let ir = ir_for_with_pipeline(
            r#"
effect resource Log;
fn helper(n: i64) -> i64 writes(Log) { n + 1 }
fn main() {
    par {
        let _ = helper(1_i64);
        let _ = helper(2_i64);
        let _ = helper(3_i64);
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert!(
            total >= 3,
            "expected ≥3 mid-branch cancel checks before effectful helper() calls across all \
             par branches, found {total}; IR:\n{ir}"
        );
    }

    #[test]
    fn test_ir_par_branch_skips_cancel_check_for_pure_callees() {
        // Pure callees (no reads/writes/sends/receives) should have their
        // mid-branch cancel checks elided per the v1 narrowing — the
        // observable behavior is unchanged because a cooperative cancel
        // can't observe a mid-state through a side-effect-free call.
        let ir = ir_for_with_pipeline(
            r#"
fn pure_helper(n: i64) -> i64 { n + 1 }
fn main() {
    par {
        let _ = pure_helper(1_i64);
        let _ = pure_helper(2_i64);
        let _ = pure_helper(3_i64);
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert_eq!(
            total, 0,
            "pure helpers should not emit mid-branch cancel checks; found {total}; IR:\n{ir}"
        );
    }

    #[test]
    fn test_ir_par_branch_mixed_pure_and_effectful() {
        // In a par block that mixes pure and effectful calls, only the
        // effectful calls should carry the cancel check.
        let ir = ir_for_with_pipeline(
            r#"
effect resource Log;
fn pure_helper(n: i64) -> i64 { n + 1 }
fn effectful_helper(n: i64) -> i64 writes(Log) { n + 1 }
fn main() {
    par {
        let _ = pure_helper(1_i64);
        let _ = effectful_helper(2_i64);
        let _ = pure_helper(3_i64);
        let _ = effectful_helper(4_i64);
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert_eq!(
            total, 2,
            "expected exactly 2 mid-branch cancel checks (one per effectful call); \
             found {total}; IR:\n{ir}"
        );
    }

    #[test]
    fn test_ir_par_branch_skips_method_check_for_pure_callee() {
        // A method whose body has no observable effects (no reads/writes/
        // sends/receives) should not emit a mid-branch cancel check at the
        // call site, mirroring the narrowing already in place for free
        // functions and `Type.assoc` calls. (`pure` is a reserved keyword
        // for future use, so the method is named `compute` here.)
        let ir = ir_for_with_pipeline(
            r#"
struct Counter { n: i64 }
impl Counter {
    fn compute(ref self) -> i64 { self.n + 1 }
}
fn main() {
    let c = Counter { n: 1 };
    par {
        let _ = c.compute();
        let _ = c.compute();
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert_eq!(
            total, 0,
            "pure method calls should not emit mid-branch cancel checks; \
             found {total}; IR:\n{ir}"
        );
    }

    #[test]
    fn test_ir_par_branch_emits_method_check_for_effectful_callee() {
        // A method that writes a resource is observably effectful — the
        // mid-branch cancel check must fire before each call site.
        let ir = ir_for_with_pipeline(
            r#"
effect resource Log;
struct Counter { n: i64 }
impl Counter {
    fn effectful(ref self) -> i64 writes(Log) { self.n + 1 }
}
fn main() {
    let c = Counter { n: 1 };
    par {
        let _ = c.effectful();
        let _ = c.effectful();
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert_eq!(
            total, 2,
            "expected exactly 2 mid-branch cancel checks (one per effectful method call); \
             found {total}; IR:\n{ir}"
        );
    }

    #[test]
    fn test_ir_non_par_function_no_cancel_check_per_call() {
        // Functions outside par blocks should NOT carry mid-call cancel
        // checks — the cancel pointer isn't even in scope.
        let ir = ir_for(
            r#"
fn helper(n: i64) -> i64 { n + 1 }
fn main() {
    let _ = helper(1_i64);
    let _ = helper(2_i64);
}
"#,
        );
        // `call.cancel.bb` is the cancel-block label emitted only by the
        // mid-branch helper. It must not appear in @main's IR.
        let start = ir.find("define i32 @main").unwrap_or(0);
        let after_main = &ir[start..];
        assert!(
            !after_main.contains("call.cancel.bb"),
            "non-par function should not emit mid-branch cancel check blocks; IR:\n{after_main}"
        );
    }

    // ── Slice 3 audit: cancel-check granularity at non-call sites ──────
    //
    // Slice 3 (Phase 7 line 67) audited every non-call IR shape in the
    // codegen pipeline against the cooperative-cancel design intent. The
    // conclusion: all v1 observable effects route through either
    // `compile_call` (`src/codegen/call_dispatch.rs:87`) or
    // `compile_method_call` (`src/codegen/method_call.rs:37`), both of
    // which emit `emit_branch_cancel_check` unconditionally at entry.
    // Non-call IR shapes — raw `build_load` / `build_store` for struct
    // field access (`compile_field_access` / `compile_field_store`),
    // index access (`compile_index_load` / `compile_index_store`),
    // tuple-extract — do NOT carry cancel checks. This matches the v1
    // semantics where effect verbs (`reads`/`writes`/`sends`/`receives`)
    // are declared on FUNCTIONS rather than per-operation: a par-branch
    // calling a `writes(R)` method gets the check at the call site, and
    // the field stores inside the callee aren't checked again. The
    // following tests pin that invariant so a future codegen change
    // (e.g. inlining a method body inline into the par-branch fn, or
    // emitting raw runtime intrinsics that bypass the call helpers)
    // doesn't silently regress the contract.

    #[test]
    fn test_ir_par_branch_field_access_does_not_add_cancel_check() {
        // Pin: raw struct field reads inside a par-branch lower to
        // `build_extract_value` / `build_struct_gep` + `build_load` and
        // do NOT contribute mid-branch cancel checks. Only the
        // surrounding effectful call site emits a check. Each branch's
        // body is a single block expression so the field reads and the
        // helper call all compile inside the same par-branch fn.
        let ir = ir_for_with_pipeline(
            r#"
effect resource Log;
fn helper(n: i64) -> i64 writes(Log) { n + 1 }
struct Pair { a: i64, b: i64 }
fn main() {
    par {
        let _ = {
            let p = Pair { a: 1_i64, b: 2_i64 };
            let r1 = p.a;
            let r2 = p.b;
            let r3 = helper(p.a);
            let r4 = p.a;
            r4
        };
        let _ = {
            let q = Pair { a: 3_i64, b: 4_i64 };
            let s1 = q.b;
            let s2 = q.a;
            let s3 = helper(q.b);
            s3
        };
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert_eq!(
            total, 2,
            "field reads inside par-branch must not add cancel checks; \
             expected exactly 2 (one per helper() call), found {total}; IR:\n{ir}"
        );
    }

    #[test]
    fn test_ir_par_branch_indexing_does_not_add_cancel_check() {
        // Pin: index access (`arr[i]`) inside a par-branch lowers to a
        // bounds-checked GEP + load and does NOT contribute mid-branch
        // cancel checks at the indexing IR site. The effectchecker
        // tracks `arr[i]` as carrying a synthetic `__builtin_index` call
        // with `panics` effect for bounds-check failure, but that
        // synthetic call is not materialised as a real LLVM call — it's
        // an inline bounds compare + abort. In v1's panic semantics,
        // panic aborts the process (no graceful Err-path interaction
        // with the cancel flag), so the missing check is by design.
        // Each branch is one block expression; the array is built
        // inside the branch so the capture story stays simple.
        let ir = ir_for_with_pipeline(
            r#"
effect resource Log;
fn helper(n: i64) -> i64 writes(Log) { n + 1 }
fn main() {
    par {
        let _ = {
            let arr = [10_i64, 20_i64, 30_i64];
            let x = arr[0];
            let y = arr[1];
            let z = helper(arr[0]);
            z
        };
        let _ = {
            let brr = [40_i64, 50_i64, 60_i64];
            let u = brr[2];
            let v = helper(brr[1]);
            v
        };
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert_eq!(
            total, 2,
            "index reads inside par-branch must not add cancel checks; \
             expected exactly 2 (one per helper() call), found {total}; IR:\n{ir}"
        );
    }

    #[test]
    fn test_ir_par_branch_literal_only_no_mid_branch_cancel_check() {
        // Pin: a par-branch whose body materialises only literals (no
        // binops, no calls, no field/index access) emits ZERO
        // mid-branch cancel checks. The entry-time check at branch
        // start still runs (it's the early-return cancellation path,
        // not counted by `count_branch_cancel_checks` which keys on
        // the `call.cancel.flag = load` instruction unique to
        // per-call-site checks). This locks in the corollary of the
        // audit: with no call sites and no non-call effect-bearing
        // IR shapes in the body, no mid-branch checks fire. Note
        // that binops like `1 + 2` are NOT literal-only — the
        // lowering pass rewrites them to `i64.add(1, 2)` calls
        // (`src/lowering.rs`), which DO trip cancel checks via the
        // standard call path, so we restrict this test to plain
        // literals.
        let ir = ir_for_with_pipeline(
            r#"
fn main() {
    par {
        let _ = 1_i64;
        let _ = 2_i64;
        let _ = 3_i64;
    }
}
"#,
        );
        let total = count_branch_cancel_checks(&ir);
        assert_eq!(
            total, 0,
            "literal-only par-branches must not emit mid-branch cancel checks; \
             found {total}; IR:\n{ir}"
        );
    }

    /// Slice 1a (Phase 7 — Par codegen: cancellation and error
    /// propagation, 2026-05-18). A par-block branch whose let-statement
    /// carries an explicit `Result[T, E]` type annotation must (a)
    /// receive a slot in a parent-allocated `__par_result_slots` array,
    /// (b) emit a slot-store of the branch's terminal Result value
    /// before `ret void`, and (c) conditionally store `i8 1` into the
    /// per-call cancel-flag pointer when the stored Result tag is
    /// `Err` (== 0 per the Result lowering convention). Sibling
    /// branches' next cooperative cancel check observes the flip.
    /// Parent-side surfacing of the Err as the par-block's value is
    /// slice 1b.
    #[test]
    fn test_ir_par_branch_result_typed_let_emits_slot_and_cancel_store() {
        let ir = ir_for(
            r#"
fn maybe_ok() -> Result[i64, i64] { Ok(42_i64) }
fn maybe_err() -> Result[i64, i64] { Err(99_i64) }
fn main() {
    par {
        let r1: Result[i64, i64] = maybe_ok();
        let r2: Result[i64, i64] = maybe_err();
    }
}
"#,
        );
        // (a) Parent allocates the Result-slot array. Result lowers to
        // `{ i64, i64, i64, i64, i64, i64 }` (tag + 5 payload words —
        // widened 2026-05-21 to four for `Result[Json, JsonError]` from
        // `Json.parse`, then to five 2026-05-30 for the client
        // `Response`'s hidden `headers` handle, phase-8 line 39); two
        // branches → `[2 x { i64, i64, i64, i64, i64, i64 }]`.
        assert!(
            ir.contains("%__par_result_slots = alloca [2 x { i64, i64, i64, i64, i64, i64 }]"),
            "expected parent-side __par_result_slots alloca \
             [2 x {{ i64, i64, i64, i64, i64, i64 }}]; IR:\n{ir}"
        );

        // (b) Each branch fn emits a slot-store. The slot pointer is
        // named `__par_result_slot_<binding>_ptr`. With two Result-
        // typed branches we expect two such GEPs across the module.
        let slot_ptrs: usize = ir
            .lines()
            .filter(|l| l.contains("__par_result_slot_") && l.contains("_ptr"))
            .count();
        assert!(
            slot_ptrs >= 2,
            "expected ≥2 result-slot GEPs across branch fns, found {slot_ptrs}; IR:\n{ir}"
        );

        // (c) Each branch fn emits a conditional cancel-flag store
        // gated on the Result tag. The conditional flow appears as
        // `__par_result_<binding>_set_cancel:` and
        // `__par_result_<binding>_after_cancel:` labels.
        let set_cancel_blocks: usize = ir
            .lines()
            .filter(|l| l.contains("__par_result_") && l.contains("_set_cancel"))
            .count();
        assert!(
            set_cancel_blocks >= 2,
            "expected ≥2 set-cancel basic blocks across branch fns, found \
             {set_cancel_blocks}; IR:\n{ir}"
        );

        // (d) Non-Result-typed par-blocks should NOT allocate the slot
        // array (it's a nullptr in the env-struct) — sanity check that
        // we didn't bloat the existing test's IR.
        let plain_ir = ir_for(
            r#"
fn main() {
    par {
        println(100);
        println(200);
    }
}
"#,
        );
        assert!(
            !plain_ir.contains("%__par_result_slots = alloca"),
            "plain par-block should not allocate the Result-slot array; IR:\n{plain_ir}"
        );
    }

    // ── End-to-end tests ──────────────────────────────────────────

    /// Slice 1a (Phase 7 — Par codegen: cancellation and error
    /// propagation, 2026-05-18). E2E smoke that a par-block with
    /// Result-typed let-statement branches compiles and runs to
    /// completion. The Err branch flips the per-call cancel flag
    /// before returning; sibling branches' next cooperative-cancel
    /// check observes the flip and short-circuits. Slice 1a does NOT
    /// surface the Err as the par-block's value (that's 1b) — the
    /// par-block here evaluates to unit and the subsequent `println`
    /// always fires. This test pins the IR's runtime correctness:
    /// the new slot-write + cancel-store ops don't crash, don't
    /// corrupt the cancel-flag pointer, and don't block the join.
    #[test]
    fn test_e2e_par_result_typed_branches_run_to_completion() {
        let out = run_program(
            r#"
fn maybe_ok() -> Result[i64, i64] { Ok(42_i64) }
fn maybe_err() -> Result[i64, i64] { Err(99_i64) }
fn main() {
    par {
        let r1: Result[i64, i64] = maybe_ok();
        let r2: Result[i64, i64] = maybe_err();
    }
    println(123_i64);
}
"#,
        );
        if let Some(out) = out {
            assert!(
                out.contains("123"),
                "post-par println should run regardless of which branch errored \
                 (slice 1a does not surface Err as par-block value); got {out:?}"
            );
        }
    }

    /// Auto-par ordered output (phase-6-runtime.md "Auto-par ordered output").
    /// Three independent `fetch_*` calls reading disjoint resources auto-
    /// parallelize into one group — each prints two trace lines AND an int.
    /// The runtime captures each branch's console output and replays it in
    /// branch (= source) order at the join, so the observable stdout is
    /// byte-identical to sequential execution: every branch's lines stay
    /// together and in source-fn order, with the post-join total last. This is
    /// the property that lets the analyzer parallelize logging-bearing work
    /// (reversing B-2026-06-13-18's blanket suppression). Covers the string
    /// path AND the int `println` primitive (both now route through the
    /// `karac_runtime_write_console` capture chokepoint).
    #[test]
    fn test_e2e_auto_par_ordered_output_preserves_source_order() {
        let out = run_program(
            r#"
effect resource DbA;
effect resource DbB;
effect resource DbC;
fn fetch_a() -> i64 reads(DbA) { println("A: start"); println("A: done"); 100_i64 }
fn fetch_b() -> i64 reads(DbB) { println("B: start"); println(2_i64); 200_i64 }
fn fetch_c() -> i64 reads(DbC) { println("C: start"); println("C: done"); 300_i64 }
fn dashboard() -> i64 reads(DbA) reads(DbB) reads(DbC) {
    let a = fetch_a();
    let b = fetch_b();
    let c = fetch_c();
    a + b + c
}
fn main() {
    let total = dashboard();
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "A: start\nA: done\nB: start\n2\nC: start\nC: done\n600\n",
                "parallelized branches' output must replay in source order, \
                 byte-identical to sequential; got {out:?}"
            );
        }
    }

    /// B-2026-07-03-4 (fixed as a side effect of B-2026-07-03-11, db020ee6):
    /// a STATIC associated fn returning `-> Self`, bound to a local and then
    /// used as a method receiver, produced `0` under the auto-par codegen path
    /// (this harness compiles WITH `concurrency_analyze`, the auto-par surface)
    /// while the sequential build and interpreter were correct (`14`). The
    /// binding `let c = W.make()` re-derived its slot type from the unrewritten
    /// `Self` shape and mis-materialized, so `c.twice()` dispatched against the
    /// wrong type. The B-11 binding-type fixes (`record_var_type_name` central
    /// resolution + the struct reverse-lookup hardening in stmts.rs) closed it;
    /// this locks in the auto-par surface the B-11 E2E tests don't cover. The
    /// static `-> Self` source is the specific trigger — the instance-`-> Self`
    /// and concrete-static (`make() -> W`) variants were already fine.
    #[test]
    fn test_e2e_auto_par_static_self_assoc_fn_receiver() {
        let out = run_program(
            r#"
struct W { v: i64 }
trait Dbl { fn twice(self) -> Self; }
impl W { fn make() -> Self { W { v: 7 } } }
impl Dbl for W { fn twice(self) -> Self { W { v: self.v + self.v } } }
fn main() {
    let c = W.make();
    let d = c.twice();
    println(d.v);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "14\n",
                "static `-> Self` assoc-fn receiver under auto-par; got {out:?}"
            );
        }
    }

    /// B-2026-07-03-32: a `Column`-producing statement grouped into an
    /// auto-par branch published its control-block pointer to the parent's
    /// return slot and THEN ran its own `FreeColumn` at branch end, so the
    /// parent read a dangling control block after the join — `c.len()`
    /// returned `0` (correct `4` under sequential build / `karac run`) and
    /// element access panicked out-of-bounds. A SILENT wrong-output
    /// miscompile. This harness compiles WITH `concurrency_analyze`, so it
    /// exercises the exact auto-par surface the default codegen E2E harness
    /// (`None` concurrency) leaves dead. The fix transfers `FreeColumn` (and
    /// its `DataFrame`/`Tensor` siblings) from branch to parent via
    /// `SlotOwnership`, mirroring the Map/Struct/SoA handles. The `hd(av)`
    /// read is the independent sibling statement that triggers the grouping.
    #[test]
    fn test_e2e_auto_par_column_producer_slot_not_freed_early() {
        let out = run_program(
            r#"
fn hd(v: Vec[i64]) -> i64 { v[0] }
fn main() {
    let av: Vec[i64] = [4, 2, 7, 1];
    println(hd(av));
    let c: Column[i64] = Column.from_vec([5, 9, 3, 1]);
    println(c.len());
    let cv: Vec[i64] = c.iter_valid();
    println(cv[3]);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "4\n4\n1\n",
                "auto-par Column producer must not free its slot-published \
                 control block; a `0` len is the dangling-read miscompile; got {out:?}"
            );
        }
    }

    /// B-2026-07-03-9 on the auto-par surface: a by-value generic `Slice[T]`
    /// param called with a Vec argument returns the correct value under
    /// auto-par too (this harness compiles WITH `concurrency_analyze`). The
    /// arg-coercion fix in `compile_generic_call` is on the shared direct-call
    /// path, so it holds regardless of whether the call's binding lands in a
    /// par group. Uses i64 and String elements — the NARROW (u8) element's
    /// print-signedness under a par group is a separate open bug
    /// (B-2026-07-03-21), so it is deliberately not exercised here.
    #[test]
    fn test_e2e_auto_par_generic_by_value_slice_param() {
        let out = run_program(
            r#"
fn gsum[T](s: Slice[T]) -> T { s[0] }
fn glen[T](s: Slice[T]) -> i64 { s.len() }
fn geti() -> i64 { 1 }
fn main() {
    let vi: Vec[i64] = [10, 20, 30];
    let vi2: Vec[i64] = [5, 6];
    let a = geti();
    let b = geti();
    println(a + b);
    println(gsum(vi));
    println(glen(vi2));
}
"#,
        );
        if let Some(out) = out {
            // i64 elements only here; the narrow (u8) print-signedness in a par
            // group is covered by test_e2e_auto_par_narrow_unsigned_slot_signedness
            // (B-2026-07-03-21, fixed) and the generic `-> T` container-element
            // String-return formatting by
            // test_e2e_auto_par_generic_slice_elem_nonint_return (B-2026-07-03-22,
            // fixed). This asserts the arg-coercion value path.
            assert_eq!(
                out, "2\n10\n2\n",
                "generic by-value Slice[T] param under auto-par; got {out:?}"
            );
        }
    }

    /// B-2026-07-03-22 on the auto-par surface: a generic `-> T` whose `T` binds
    /// from a `Slice[T]` param element must resolve to the concrete (non-`i64`)
    /// element type so the returned String formats correctly. Compiled through
    /// the auto-par analyzer (`Some(&analysis)`); the value path itself is
    /// analyzer-independent, but this guards the fix on the DEFAULT build too.
    #[test]
    fn test_e2e_auto_par_generic_slice_elem_nonint_return() {
        let out = run_program(
            r#"
fn gsum[T](s: Slice[T]) -> T { s[0] }
fn main() {
    let vs: Vec[String] = ["autopar-first-long-payload-here", "autopar-second-long-payload"];
    println(f"{gsum(vs)}");
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "autopar-first-long-payload-here\n",
                "generic Slice[T] non-int element return under auto-par; got {out:?}"
            );
        }
    }

    /// B-2026-07-03-21: a narrow *unsigned* (u8/u16/u32) local whose RHS is a
    /// generic call, bound with an explicit annotation, must keep its
    /// signedness when the binding lands in an auto-par PAR GROUP. The
    /// post-join return-slot materialization registered `variables` /
    /// `vec_elem_types` but never `var_type_names`, and the slot's `llvm_ty`
    /// (`i8`/`i16`/`i32`) erases signedness — so `expr_is_unsigned_int` fell
    /// back to signed and `255u8` printed as `-1` under the DEFAULT/auto-par
    /// build (KARAC_AUTO_PAR=1), while sequential + interp printed `255`. The
    /// fix threads the binding's annotation type name into `ReturnSlot` and
    /// re-registers `var_type_names` post-join. Covers u8/u16/u32 (plain print
    /// and f-string) plus a signed `i8` control that must STAY `-1` (no
    /// over-correction). The two `geti()` lets create the independent-binding
    /// dependency graph the analyzer needs to group the narrow-int let.
    #[test]
    fn test_e2e_auto_par_narrow_unsigned_slot_signedness() {
        let out = run_program(
            r#"
fn vfirst[T](v: ref Vec[T]) -> T { v[0] }
fn geti() -> i64 { 1 }
fn main() {
    let vu8: Vec[u8] = [255u8, 1u8];
    let vu16: Vec[u16] = [65535u16, 1u16];
    let vu32: Vec[u32] = [4294967295u32, 1u32];
    let vi8: Vec[i8] = [255u8 as i8, 1u8 as i8];
    let a = geti();
    let b = geti();
    let u8v: u8 = vfirst(vu8);
    let u16v: u16 = vfirst(vu16);
    let u32v: u32 = vfirst(vu32);
    let i8v: i8 = vfirst(vi8);
    println(a + b);
    println(u8v);
    println(f"{u16v}");
    println(u32v);
    println(i8v);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "2\n255\n65535\n4294967295\n-1\n",
                "narrow-unsigned generic-call slot signedness under auto-par; got {out:?}"
            );
        }
    }

    /// B-2026-07-03-7 (codegen side), auto-par surface: `Vec[Struct].sort()`
    /// and `Vec[Enum].sort()` for a `#[derive(Ord)]` user type resolve the
    /// `karac_cmp_<T>` declaration-order comparator identically under DEFAULT
    /// auto-par (the cmp-fn family is effect-free, so the sort is unaffected by
    /// parallelization) — third A/B surface for the ordering fix.
    #[test]
    fn test_e2e_auto_par_struct_enum_sort_declaration_order() {
        let out = run_program(
            r#"
#[derive(Eq, Ord)]
struct Rect { width: i64, height: i64 }
#[derive(Eq, Ord)]
enum Shape { Circle(i64), Rect(i64, i64), Unit }
fn stag(s: Shape) -> i64 {
    match s { Shape.Circle(r) => 100 + r, Shape.Rect(w, h) => 200 + w * 10 + h, Shape.Unit => 300 }
}
fn main() {
    let mut v: Vec[Rect] = Vec.new();
    v.push(Rect { width: 2, height: 1 });
    v.push(Rect { width: 1, height: 9 });
    v.sort();
    let mut i = 0;
    while i < v.len() { let r = v[i]; println(f"{r.width},{r.height}"); i = i + 1; }
    let mut s: Vec[Shape] = Vec.new();
    s.push(Shape.Rect(2, 1));
    s.push(Shape.Circle(9));
    s.push(Shape.Unit);
    s.push(Shape.Circle(3));
    s.sort();
    let mut j = 0;
    while j < s.len() { let sh = s[j]; println(f"{stag(sh)}"); j = j + 1; }
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "1,9\n2,1\n103\n109\n221\n300\n",
                "struct/enum sort declaration order under auto-par; got {out:?}"
            );
        }
    }

    /// B-2026-07-03-7 (codegen side), auto-par surface: `<`/`<=`/`>`/`>=` on a
    /// `#[derive(Ord)]` struct/enum lower through the effect-free
    /// `karac_cmp_<T>` comparator identically under DEFAULT auto-par.
    #[test]
    fn test_e2e_auto_par_ordered_operators_struct_enum() {
        let out = run_program(
            r#"
#[derive(Eq, Ord)]
struct P { a: i64, b: i64 }
#[derive(Eq, Ord)]
enum Pri { Low, Med, High }
fn main() {
    let x = P { a: 1, b: 2 };
    let y = P { a: 1, b: 3 };
    println(f"{x < y}");
    println(f"{y > x}");
    println(f"{y < x}");
    println(f"{Pri.Low < Pri.High}");
    println(f"{Pri.High < Pri.Low}");
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "true\ntrue\nfalse\ntrue\nfalse\n",
                "ordered operators on struct/enum under auto-par; got {out:?}"
            );
        }
    }

    /// B-2026-06-19-13 codegen follow-on, auto-par surface: `char.to_digit`
    /// lowers to a pure branch-select + Option constructor regardless of
    /// parallelization (it's effect-free), so DEFAULT auto-par agrees.
    #[test]
    fn test_e2e_auto_par_char_to_digit() {
        let out = run_program(
            r#"
fn show(c: char, r: u32) {
    match c.to_digit(r) {
        Some(v) => println(f"{v}"),
        None => println("none"),
    }
}
fn main() {
    show('7', 10);
    show('a', 16);
    show('z', 36);
    show('9', 2);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "7\n10\n35\nnone\n",
                "char.to_digit under auto-par; got {out:?}"
            );
        }
    }

    #[test]
    fn test_e2e_auto_par_map_histogram_then_keys_no_race() {
        // `for w in words { *m.entry(w).or_insert(0) += 1 }` WRITES the map,
        // then `m.keys()` reads it. Auto-par must serialize the loop against the
        // read — pre-fix the loop's writes(m) was invisible (a deref-of-method-
        // chain assign target recorded no write), so the loop and keys() were
        // parallelized and raced on the map (crash / garbled output under the
        // DEFAULT auto-par build; B-2026-06-20-16). End-to-end over the whole
        // word-frequency cascade (moved-loop-elem copy, keys deep-copy, String
        // sort, and this auto-par serialization); deterministic sorted report.
        let out = run_program(
            r#"
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("ccc-key".to_string());
    words.push("aaa-key".to_string());
    words.push("ccc-key".to_string());
    words.push("bbb-key".to_string());
    words.push("aaa-key".to_string());
    words.push("ccc-key".to_string());
    let mut m: Map[String, i64] = Map.new();
    for w in words {
        *m.entry(w).or_insert(0_i64) += 1_i64;
    }
    let mut ks: Vec[String] = m.keys();
    ks.sort();
    for k in ks {
        println(f"{k}={m.get_or(k.clone(), 0_i64)}");
    }
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out, "aaa-key=2\nbbb-key=1\nccc-key=3\n");
        }
    }

    #[test]
    fn test_e2e_auto_par_sort_and_pop_serialize_against_reads() {
        // `v.sort()` / `v.pop()` / `v.remove(i)` WRITE the receiver, and the
        // following reads of `v` must serialize against them under the DEFAULT
        // auto-par build. Pre-fix, `method_effects_imply_receiver_mutation`
        // found no effect key for `sort`/`pop`/`remove` (only `push`/`insert`
        // family methods were seeded in the effectchecker's builtin table), so
        // the analyzer saw no data dependency and raced the mutation against
        // the read:
        //   - sort: non-deterministic duplicated/lost elements (the read ran
        //     mid permute-back) — a 2-element Vec[String] printed
        //     `omega,omega` on ~25% of runs;
        //   - pop/remove: 100% deterministic stale read (the branch captured
        //     a pre-mutation {ptr,len,cap} header copy), printing the
        //     un-popped len.
        // (B-2026-07-02-8; same class as B-2026-06-20-16's histogram write.)
        // Multiple independent sections make the group former's job easy —
        // each section's internal chain is what must stay ordered.
        let out = run_program(
            r#"
fn main() {
    let mut v1: Vec[String] = Vec.new();
    v1.push("zeta".to_string()); v1.push("alpha".to_string());
    v1.sort();
    for x in v1 { println(x); }

    let mut v2: Vec[String] = Vec.new();
    v2.push("delta".to_string()); v2.push("beta".to_string());
    v2.sort();
    for x in v2 { println(x); }

    let mut v3: Vec[i64] = Vec.new();
    v3.push(1_i64); v3.push(2_i64); v3.push(3_i64);
    v3.pop();
    println(v3.len());

    let mut v4: Vec[i64] = Vec.new();
    v4.push(8_i64); v4.push(9_i64);
    v4.remove(0_i64);
    println(v4.len());
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "alpha\nzeta\nbeta\ndelta\n2\n1\n",
                "sort/pop/remove must serialize against subsequent reads \
                 under the default auto-par build; got {out:?}"
            );
        }
    }

    /// Slice 1b (Phase 7 — Par codegen: cancellation and error
    /// propagation, 2026-05-20). A par-block with Result-typed
    /// branches AND a join expression emits a parent-side err-walk
    /// that loads each slot's tag in branch-index order, branches on
    /// Err to a per-slot found block, and phi-merges the first errored
    /// slot's Result against the join expression's value at the
    /// par-block exit. Pins the new IR shape: per-slot check + found
    /// labels, the compile-join label, the exit label, and the phi
    /// instruction with `__par_block_value` as its SSA name.
    #[test]
    fn test_ir_par_block_result_typed_join_emits_err_pick_and_phi() {
        let ir = ir_for(
            r#"
fn maybe_ok(v: i64) -> Result[i64, i64] { Ok(v) }
fn main() {
    let r: Result[i64, i64] = par {
        let r1: Result[i64, i64] = maybe_ok(10_i64);
        let r2: Result[i64, i64] = maybe_ok(20_i64);
        Ok(0_i64)
    };
}
"#,
        );
        // Slice 2 (2026-05-21): single-load err-pick path replaces
        // slice 1b's per-slot tag walk. One check + one err-found BB
        // (no per-index suffix).
        assert!(
            ir.contains("__par_err_check:"),
            "expected __par_err_check label; IR:\n{ir}"
        );
        assert!(
            ir.contains("__par_err_found:"),
            "expected __par_err_found label; IR:\n{ir}"
        );
        // The slice-1b per-index labels must no longer appear — if
        // they do, the parent reverted to the walk.
        assert!(
            !ir.contains("__par_err_check_0:"),
            "slice 2 should not emit per-slot err-check labels; IR:\n{ir}"
        );
        assert!(
            !ir.contains("__par_err_found_0:"),
            "slice 2 should not emit per-slot err-found labels; IR:\n{ir}"
        );
        // Branch fns CAS-min into the parent's i32 cell. Pin the
        // atomicrmw umin shape (`atomicrmw umin ptr ..., i32 ..., monotonic`).
        assert!(
            ir.contains("atomicrmw umin"),
            "expected atomicrmw umin on the earliest-err-idx cell; IR:\n{ir}"
        );
        // Compile-join and exit BBs.
        assert!(
            ir.contains("__par_compile_join:"),
            "expected __par_compile_join label; IR:\n{ir}"
        );
        assert!(
            ir.contains("__par_block_exit:"),
            "expected __par_block_exit label; IR:\n{ir}"
        );
        // Phi at exit, named `__par_block_value`. Two incoming
        // entries — err-found + compile_join — vs slice 1b's
        // N + 1.
        assert!(
            ir.contains("__par_block_value = phi"),
            "expected __par_block_value phi instruction; IR:\n{ir}"
        );

        // Negative side: a Result-typed-branch par-block WITHOUT a
        // join expression should not emit the err-pick / phi — slice
        // 1a's behavior is preserved (par-block evaluates to unit).
        let no_join_ir = ir_for(
            r#"
fn maybe_ok() -> Result[i64, i64] { Ok(42_i64) }
fn main() {
    par {
        let r1: Result[i64, i64] = maybe_ok();
        let r2: Result[i64, i64] = maybe_ok();
    }
}
"#,
        );
        assert!(
            !no_join_ir.contains("__par_err_check"),
            "no-join par-block should not emit err-pick BBs; IR:\n{no_join_ir}"
        );
        assert!(
            !no_join_ir.contains("__par_block_value = phi"),
            "no-join par-block should not emit the exit phi; IR:\n{no_join_ir}"
        );
    }

    /// Slice 1b (2026-05-20). E2E: when every par-block branch's
    /// Result-typed let-binding is Ok, the par-block evaluates to the
    /// join expression's value. The err-walk loads each tag, every
    /// comparison fails (tag == 1 for Ok), and control falls through
    /// to `__par_compile_join` whose `Ok(7_i64)` is what the phi
    /// surfaces at exit. Confirms the Ok-only path doesn't accidentally
    /// short-circuit to a slot's Result.
    #[test]
    fn test_e2e_par_block_result_ok_only_returns_join_expression() {
        let out = run_program(
            r#"
fn maybe_ok(v: i64) -> Result[i64, i64] { Ok(v) }
fn main() {
    let r: Result[i64, i64] = par {
        let r1: Result[i64, i64] = maybe_ok(10_i64);
        let r2: Result[i64, i64] = maybe_ok(20_i64);
        Ok(7_i64)
    };
    match r {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "7",
                "Ok-only par-block should surface the join expression's Ok value; \
                 got {out:?}"
            );
        }
    }

    /// Slice 1b (2026-05-20). E2E: when one of the par-block branches
    /// returns Err, the par-block surfaces that Err *instead of* the
    /// join expression's value. The err-walk's slot-0 / slot-1 check
    /// finds the errored slot, jumps into `__par_err_found_<i>`, and
    /// the phi at exit picks the slot's loaded Result rather than
    /// `Ok(7_i64)` from the join. Pin: the printed value must match
    /// the Err payload (`99`), not the join expression's payload (`7`).
    #[test]
    fn test_e2e_par_block_result_single_err_overrides_join() {
        let out = run_program(
            r#"
fn maybe_ok(v: i64) -> Result[i64, i64] { Ok(v) }
fn maybe_err(v: i64) -> Result[i64, i64] { Err(v) }
fn main() {
    let r: Result[i64, i64] = par {
        let r1: Result[i64, i64] = maybe_ok(10_i64);
        let r2: Result[i64, i64] = maybe_err(99_i64);
        Ok(7_i64)
    };
    match r {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "99",
                "single-Err par-block should surface the slot's Err value, \
                 not the join expression's Ok; got {out:?}"
            );
        }
    }

    /// Slice 2 (2026-05-21). Source-order error reporting: when
    /// multiple branches err, the par-block must surface the
    /// source-order earliest one regardless of which branch's Err
    /// landed in its slot first. Branch 0 returns Err(11) after
    /// burning CPU; branch 1 returns Err(33) before it. A naive
    /// "first slot to be written wins" implementation would print
    /// 33; the slice-2 atomicrmw-umin path must print 11.
    ///
    /// Load-immunity protocol (bugs.md flake, fixed 2026-06-06). The
    /// original shape (free-running spin in branch 0, immediate Err
    /// in branch 1) flaked under full-suite CPU saturation
    /// (reproduced ~1/50 under 24-way load): branch 0's worker could
    /// start late enough that branch 1's recorded Err had already
    /// set the cooperative-cancel flag, branch 0 was then cancelled
    /// at its branch-start / pre-call check and never recorded
    /// Err(11) — and per design.md § Parallel Failure and Cleanup
    /// (join-then-cleanup step 3: a cancelled sibling "abandons its
    /// current work"; source-order precedence applies among errors
    /// actually recorded), 33 winning in that schedule is correct
    /// semantics, so the old test over-specified. The gate makes the
    /// race one-directional: branch 1 holds its Err(33) until
    /// branch 0 has entered `slow_err` — i.e. until branch 0 is past
    /// its last cooperative-cancel point (checks exist at branch
    /// start and before branch-body call sites only, never inside a
    /// callee). No error can exist before `slow_err` is entered, so
    /// branch 0's checks always pass and Err(11) is always recorded;
    /// branch 1's Err(33) then lands first in wall-clock while 11
    /// still must win by source order. Deterministic under any
    /// scheduler load (verified 30/30 under 20-way saturation). The
    /// suppressed-Err schedule is pinned by the companion test
    /// `test_e2e_par_block_cancelled_branch_err_not_recorded` below.
    #[test]
    fn test_e2e_par_block_result_source_order_earliest_branch_wins() {
        let out = run_program(
            r#"
par struct Gate { v: Atomic[i64] }
impl Gate {
    fn open(ref self) { self.v.store(1, MemoryOrdering.SeqCst) }
    fn is_open(ref self) -> bool { self.v.load(MemoryOrdering.SeqCst) == 1 }
}

fn slow_err(v: i64, g: Gate) -> Result[i64, i64] {
    g.open();
    let mut acc: i64 = 0_i64;
    let mut i: i64 = 0_i64;
    while i < 1000000_i64 {
        acc = acc + i;
        i = i + 1_i64;
    }
    if acc < 0_i64 {
        Ok(acc)
    } else {
        Err(v)
    }
}

fn gated_err(v: i64, g: Gate) -> Result[i64, i64] {
    while not g.is_open() { }
    Err(v)
}

fn main() {
    let g = Gate { v: Atomic.new(0) };
    let r: Result[i64, i64] = par {
        let r1: Result[i64, i64] = slow_err(11_i64, g);
        let r2: Result[i64, i64] = gated_err(33_i64, g);
        Ok(7_i64)
    };
    match r {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "11",
                "source-order earliest branch (idx 0, Err(11)) must win \
                 over later-source-order branch (idx 1, Err(33)) even \
                 when the later branch lands in its slot first; \
                 got {out:?}"
            );
        }
    }

    /// Companion pinning the cancellation side of the source-order
    /// rule (design.md § Parallel Failure and Cleanup): a sibling
    /// cancelled before recording its own Err contributes nothing to
    /// the scope's return value — source-order precedence applies
    /// among errors *actually recorded*, not errors a branch would
    /// have produced had it not been cancelled. Branch 0
    /// (source-earliest) spins on a gate that is never opened, so
    /// its only exit is the cooperative-cancel check emitted before
    /// the in-loop `g.is_open()` call; branch 1's immediate Err(33)
    /// sets the cancel flag, branch 0 abandons without ever reaching
    /// `would_err(11)`, and the scope deterministically returns 33.
    /// Doubles as a liveness check that cooperative cancellation
    /// reaches a busy sibling (if the in-loop cancel check were
    /// elided — e.g. by callee-effectful narrowing misclassifying
    /// the atomic-load method — this test would hang, not fail).
    #[test]
    fn test_e2e_par_block_cancelled_branch_err_not_recorded() {
        let out = run_program(
            r#"
par struct Gate { v: Atomic[i64] }
impl Gate {
    fn is_open(ref self) -> bool { self.v.load(MemoryOrdering.SeqCst) == 1 }
}

fn fast_err(v: i64) -> Result[i64, i64] { Err(v) }

fn would_err(v: i64) -> Result[i64, i64] {
    if v < 0 {
        return Ok(v);
    }
    Err(v)
}

fn main() {
    let g = Gate { v: Atomic.new(0) };
    let r: Result[i64, i64] = par {
        let r1: Result[i64, i64] = {
            while not g.is_open() { }
            would_err(11_i64)
        };
        let r2: Result[i64, i64] = fast_err(33_i64);
        Ok(7_i64)
    };
    match r {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "33",
                "a branch cancelled before recording its Err must not \
                 contribute to the scope's return value — the recorded \
                 Err(33) wins even though a source-earlier branch would \
                 have erred with 11; got {out:?}"
            );
        }
    }

    /// Slice 2 (2026-05-21). Companion to the above: when branch 0
    /// returns Ok and only branch 1 (the second-source-order
    /// Result-typed branch) errs, that branch's Err is what surfaces
    /// — the sentinel-vs-min comparison must not spuriously pick the
    /// Ok slot. Same shape as
    /// `test_e2e_par_block_result_single_err_overrides_join` but with
    /// the err on the later branch, sanity-checking the umin direction.
    #[test]
    fn test_e2e_par_block_result_later_branch_err_still_surfaces() {
        let out = run_program(
            r#"
fn maybe_ok(v: i64) -> Result[i64, i64] { Ok(v) }
fn maybe_err(v: i64) -> Result[i64, i64] { Err(v) }
fn main() {
    let r: Result[i64, i64] = par {
        let r1: Result[i64, i64] = maybe_ok(10_i64);
        let r2: Result[i64, i64] = maybe_ok(20_i64);
        let r3: Result[i64, i64] = maybe_err(42_i64);
        Ok(7_i64)
    };
    match r {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out.trim(),
                "42",
                "only-late-branch Err must surface its Err value, not the \
                 join expression's Ok; got {out:?}"
            );
        }
    }

    #[test]
    fn test_e2e_par_both_branches_run() {
        let out = run_program(
            r#"
fn main() {
    par {
        println(100);
        println(200);
    }
}
"#,
        );
        if let Some(out) = out {
            // Branches may interleave — just verify both tokens appear.
            assert!(
                out.contains("100"),
                "first branch should have printed 100; got {out:?}"
            );
            assert!(
                out.contains("200"),
                "second branch should have printed 200; got {out:?}"
            );
        }
    }

    #[test]
    fn test_e2e_par_three_branches_run() {
        let out = run_program(
            r#"
fn main() {
    par {
        println(1);
        println(2);
        println(3);
    }
}
"#,
        );
        if let Some(out) = out {
            for tok in ["1", "2", "3"] {
                assert!(
                    out.contains(tok),
                    "branch {tok} should have printed; got {out:?}"
                );
            }
        }
    }

    /// Bug #6 regression: explicit `par {}` blocks support the
    /// canonical "branches define let-bindings; join expression
    /// combines them" shape from `docs/syntax.md § 5.9` and
    /// `docs/design.md § Explicit Concurrency`:
    ///
    /// ```kara
    /// let (x, y) = par {
    ///     let p = double(a)
    ///     let o = double(b)
    ///     (p, o)
    /// }
    /// ```
    ///
    /// Pre-fix `compile_par_block` passed an empty slot list to
    /// `emit_par_run`, so the branches' let-bindings stayed
    /// branch-local — the final-expression `(p, o)` then read names
    /// not visible in the parent scope and errored with "Undefined
    /// variable 'p'". Fix walks the final expression for references
    /// to branch-defined names, materializes a `ReturnSlot` per
    /// match (parallel to the auto-par dispatch site's
    /// `compute_return_slots_checked`), threads them through, and
    /// binds each loaded value as a parent-scope local before
    /// compiling the join expression.
    #[test]
    fn test_e2e_par_block_join_expression_reads_branch_bindings() {
        let out = run_program(
            r#"
fn double(x: i64) -> i64 { x * 2 }
fn main() {
    let a: i64 = 10;
    let b: i64 = 20;
    let (x, y) = par {
        let x = double(a);
        let y = double(b);
        (x, y)
    };
    println(x + y);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "60\n",
                "par-block join expression must see let-introduced bindings from the \
                 branches — the canonical doc example must compile and print 60"
            );
        }
    }

    /// Bug #6 follow-up: richer `par {}` block with four branches
    /// each binding a value, summed at the join. Stress-tests slot
    /// layout determinism, branch-to-parent type propagation for
    /// multiple slots, and that the join expression's binary-op
    /// chain reads each slot correctly.
    #[test]
    fn test_e2e_par_block_join_sums_four_branch_results() {
        let out = run_program(
            r#"
fn compute_a(n: i64) -> i64 {
    let mut sum: i64 = 0;
    let mut i: i64 = 0;
    while i < n {
        sum = sum + i;
        i = i + 1;
    }
    sum
}
fn compute_b(n: i64) -> i64 {
    let mut prod: i64 = 1;
    let mut i: i64 = 1;
    while i <= n {
        prod = prod + i * i;
        i = i + 1;
    }
    prod
}
fn main() {
    let n: i64 = 10;
    let m: i64 = 5;
    let total = par {
        let a = compute_a(n);
        let b = compute_b(n);
        let c = compute_a(m);
        let d = compute_b(m);
        a + b + c + d
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            // compute_a(10)=45, compute_b(10)=386, compute_a(5)=10,
            // compute_b(5)=56 → 45+386+10+56 = 497
            assert_eq!(
                out, "497\n",
                "par-block join must sum four branch results deterministically; \
                 each branch binding must propagate through its own slot"
            );
        }
    }

    // ── Auto-parallelization of non-par regions ──

    /// Compile-time helper for slice 2's auto-par tests: runs the full
    /// pipeline (resolve → typecheck → lower → effectcheck →
    /// concurrency_analyze), threads the resulting `ConcurrencyAnalysis`
    /// into codegen via `compile_to_ir_with_options`, and returns the
    /// emitted IR. Mirrors `ir_for_with_pipeline` but additionally
    /// constructs the analysis object the auto-par codegen path consumes.
    fn ir_for_with_concurrency(src: &str) -> String {
        use karac::codegen::compile_to_ir_with_options;
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

    /// Three independent reads on disjoint resources — the analyzer
    /// groups all three as parallelizable, no binding leaks out (all
    /// `let _ = ...`), so the auto-par dispatch fires and the IR holds
    /// exactly one `karac_par_run` call site that fans out three branch
    /// fns.
    #[test]
    fn test_auto_par_three_independent_reads_emits_par_run() {
        let ir = ir_for_with_concurrency(
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
        let calls = ir.matches("call void @karac_par_run").count();
        assert_eq!(
            calls, 1,
            "expected exactly one karac_par_run dispatch for three independent reads; \
             found {calls}; IR:\n{ir}"
        );
        // Three branch fns minted from one auto-par site. We use
        // par_id=0 because main's body is the first par site emitted.
        for i in 0..3 {
            let needle = format!("__par_branch_0_{i}");
            assert!(
                ir.contains(&needle),
                "expected branch fn {needle} in IR:\n{ir}"
            );
        }
    }

    /// Three pure top-level lets — the analyzer marks the group as
    /// `is_trivial = true` (no effects), and the codegen granularity
    /// gate emits sequentially with no `karac_par_run` call. Pins the
    /// `is_trivial` short-circuit in `compile_function_body`.
    #[test]
    fn test_auto_par_skips_trivial_pure_group() {
        let ir = ir_for_with_concurrency(
            r#"
fn main() {
    let _a = 1_i64;
    let _b = 2_i64;
    let _c = 3_i64;
}
"#,
        );
        assert!(
            !ir.contains("call void @karac_par_run"),
            "trivial pure group should not call karac_par_run; IR:\n{ir}"
        );
    }

    /// Two `writes(Disk)` calls on the same resource — the analyzer
    /// must not group them (effect conflict on the same resource), so
    /// the codegen emits sequentially. Pins that the lowering respects
    /// analyzer decisions and never speculatively parallelizes.
    #[test]
    fn test_auto_par_serializes_when_resources_conflict() {
        let ir = ir_for_with_concurrency(
            r#"
effect resource Disk;

fn write_a() -> i64 writes(Disk) { 1 }
fn write_b() -> i64 writes(Disk) { 2 }

fn main() {
    let _ = write_a();
    let _ = write_b();
}
"#,
        );
        assert!(
            !ir.contains("call void @karac_par_run"),
            "writes(Disk) ↔ writes(Disk) should serialize; IR:\n{ir}"
        );
    }

    /// Captured-mutation safety net: when a multi-stmt par-eligible
    /// group mutates pre-existing locals (here `a` and `b` via mutating
    /// methods on disjoint user-defined effect resources) and those
    /// locals are read after the group, codegen must bail to sequential.
    /// `karac_par_run` captures locals by value into the per-branch env
    /// struct, so a branch's mutation lands on the bit-copy and is lost
    /// at join time — only let-introduced bindings flow back through the
    /// return-slot mechanism. Without this gate, `a.n` would still read
    /// `0` after the par-run "completed" the bumps.
    ///
    /// Detection lives in the analyzer (`StmtInfo.defines −
    /// StmtInfo.let_introduced`, unioned across group stmts as
    /// `ParallelGroup.captured_mutations`); codegen consults the field
    /// at `compute_return_slots_checked` and returns `None` (sequential
    /// fallback) when it overlaps with the names read outside the group.
    /// Pinning the absence of `karac_par_run` here regression-locks both
    /// the analyzer's set computation and the codegen consumption.
    #[test]
    fn test_auto_par_bails_when_captured_mutation_read_after_group() {
        let ir = ir_for_with_concurrency(
            r#"
effect resource R1;
effect resource R2;

struct Counter { n: i64 }

impl Counter {
    fn bump_a(mut ref self) writes(R1) {
        self.n = self.n + 1;
    }
    fn bump_b(mut ref self) writes(R2) {
        self.n = self.n + 1;
    }
}

fn main() {
    let mut a: Counter = Counter { n: 0 };
    let mut b: Counter = Counter { n: 0 };
    a.bump_a();
    b.bump_b();
    let _x: i64 = a.n;
    let _y: i64 = b.n;
}
"#,
        );
        assert!(
            !ir.contains("call void @karac_par_run"),
            "captured-mutation read after group must bail to sequential; \
             without the bail, `a` and `b` would be bit-copied into the \
             per-branch env and the bumps would silently land on the \
             local copies. IR:\n{ir}"
        );
    }

    // ── Auto-parallelization with return values ──
    //
    // Slice A (Phase-7 — Par codegen: return values, 2026-05-09) lifts
    // slice 2's `group_defines_binding_used_outside` gate. Each parallel
    // group whose let-bindings are read after the group gets a
    // synthesized parent-allocated return struct
    // (`__karac_ParGroup_<spawn_site_id>_Returns`); each branch writes
    // its produced value into an assigned field by offset, and the
    // parent loads the values back after `karac_par_run` joins. The
    // first test below confirms the four-read demo shape — which under
    // slice 2 fell back to sequential — now fans out through one
    // `karac_par_run` call with four branch fns and produces four
    // `load` instructions for the slot-back-read. The second test
    // pins the parallax-lite shape (no class-(ii) bindings) at byte-
    // equivalent IR — the empty-slot path should preserve slice 2's
    // behavior exactly.

    /// Four-read demo shape: each branch produces a typed value, the
    /// final-expr call consumes them. Asserts: (a) exactly one
    /// `karac_par_run` dispatch, (b) the parent allocates a
    /// `__karac_ParGroup_*_Returns` struct, (c) four slot-load
    /// instructions appear after the runtime call, (d) the joined
    /// `combine` call site receives those loaded values as its args.
    #[test]
    fn test_auto_par_four_reads_with_join_emits_par_run_and_slot_loads() {
        let ir = ir_for_with_concurrency(
            r#"
effect resource Net;
effect resource Disk;
effect resource Db;
effect resource Cache;

fn fetch_net() -> i64 reads(Net) { 1 }
fn fetch_disk() -> i64 reads(Disk) { 2 }
fn fetch_db() -> i64 reads(Db) { 3 }
fn fetch_cache() -> i64 reads(Cache) { 4 }

fn combine(a: i64, b: i64, c: i64, d: i64) -> i64 {
    a + b + c + d
}

fn main() {
    let result_1 = fetch_net();
    let result_2 = fetch_disk();
    let result_3 = fetch_db();
    let result_4 = fetch_cache();
    println(combine(result_1, result_2, result_3, result_4));
}
"#,
        );
        // (a) Exactly one karac_par_run dispatch.
        let calls = ir.matches("call void @karac_par_run").count();
        assert_eq!(
            calls, 1,
            "expected exactly one karac_par_run dispatch for four-read fan-out; \
             found {calls}; IR:\n{ir}"
        );
        // Four branch fns minted from the one auto-par site.
        for i in 0..4 {
            let needle = format!("__par_branch_0_{i}");
            assert!(
                ir.contains(&needle),
                "expected branch fn {needle} in IR:\n{ir}"
            );
        }
        // (b) Return struct synthesized with a deterministic name.
        // The name is `__karac_ParGroup_<id>_Returns`; the first auto-
        // par site mints id=0.
        assert!(
            ir.contains("__karac_ParGroup_0_Returns"),
            "expected return-struct type `__karac_ParGroup_0_Returns` in IR:\n{ir}"
        );
        // (c) Four slot loads after `karac_par_run` — one per slot.
        // The IR uses one `getelementptr` per slot field address plus
        // one `load` per field; we look for the named result registers
        // we emitted (`%result_1`, `%result_2`, `%result_3`,
        // `%result_4`) which only appear at the slot-back-read sites
        // because the original source bindings are inside the branches
        // (not visible in the parent function before slice A landed).
        for name in ["result_1", "result_2", "result_3", "result_4"] {
            let needle = format!("__par_slot_{name}_ptr");
            assert!(
                ir.contains(&needle),
                "expected slot-pointer GEP {needle} in IR:\n{ir}"
            );
        }
        // Four slot loads: each binding name appears as a load result.
        // Allow either the value-form or the pointer-load shape; the
        // simple "load i64" count must equal-or-exceed 4 inside main.
        let load_count = ir.matches("__par_slot_result_").count();
        assert!(
            load_count >= 4,
            "expected ≥4 slot-related GEP/load registers; found {load_count}; IR:\n{ir}"
        );
        // (d) The `combine(...)` call site uses the loaded values.
        // The parent emits exactly one `call i64 @combine(...)` after
        // the par-run dispatch, with arguments fed from the four slot
        // loads. We assert the call exists; the argument-flow
        // assertion is covered by the E2E correctness test in
        // tests/codegen.rs (wall-clock + sum-value).
        assert!(
            ir.contains("call i64 @combine"),
            "expected `call i64 @combine` site after slot loads in IR:\n{ir}"
        );
    }

    /// Bug fix: when an auto-par group's bindings are consumed only
    /// via an f-string in the tail position (e.g.
    /// `f"{a}-{b}-{c}-{d}"`), `compute_return_slots`'s `refs_in_expr`
    /// walk previously didn't recurse into
    /// `ExprKind::InterpolatedStringLit`'s segment list, so the four
    /// names were missed by the outside-group reads set, no slots
    /// were materialized, and codegen errored "Undefined variable"
    /// at the f-string's load sites (the workaround applied in
    /// Slice E `ea1d26d` was returning a fixed JSON literal).
    ///
    /// Fixed in `src/codegen.rs::refs_in_expr`: added an
    /// `InterpolatedStringLit` arm that walks each
    /// `ParsedInterpolationPart::Expr`. This test mirrors the
    /// four-reads-with-join shape above but routes the captures
    /// through an f-string instead of a direct fn call.
    #[test]
    fn test_auto_par_fstring_tail_captures_par_group_bindings() {
        let ir = ir_for_with_concurrency(
            r#"
effect resource Net;
effect resource Disk;
effect resource Db;
effect resource Cache;

fn fetch_net() -> i64 reads(Net) { 1 }
fn fetch_disk() -> i64 reads(Disk) { 2 }
fn fetch_db() -> i64 reads(Db) { 3 }
fn fetch_cache() -> i64 reads(Cache) { 4 }

fn main() {
    let result_1 = fetch_net();
    let result_2 = fetch_disk();
    let result_3 = fetch_db();
    let result_4 = fetch_cache();
    println(f"{result_1}-{result_2}-{result_3}-{result_4}");
}
"#,
        );
        let calls = ir.matches("call void @karac_par_run").count();
        assert_eq!(calls, 1, "expected one karac_par_run dispatch; IR:\n{ir}");
        assert!(
            ir.contains("__karac_ParGroup_0_Returns"),
            "expected return struct; pre-fix the f-string segments \
             weren't walked so no slots were materialized; IR:\n{ir}"
        );
        for name in ["result_1", "result_2", "result_3", "result_4"] {
            let needle = format!("__par_slot_{name}_ptr");
            assert!(
                ir.contains(&needle),
                "expected slot-pointer GEP {needle}; IR:\n{ir}"
            );
        }
    }

    /// Three independent `writes(R_i)` calls on disjoint resources
    /// with no joined return — the parallax-lite microbenchmark
    /// shape. The auto-par dispatch fires (one `karac_par_run`, three
    /// branches) but the slot mechanism is dormant: no return-struct
    /// type is emitted, no slot-pointer GEPs. Pins that the empty-slot
    /// path preserves slice 2's behavior — the load-bearing test
    /// against IR-shape regression for the parallax-lite benchmark.
    #[test]
    fn test_auto_par_three_reads_no_outside_use_keeps_parallax_lite_shape() {
        let ir = ir_for_with_concurrency(
            r#"
effect resource R0;
effect resource R1;
effect resource R2;

fn write_r0() writes(R0) {}
fn write_r1() writes(R1) {}
fn write_r2() writes(R2) {}

fn main() {
    write_r0();
    write_r1();
    write_r2();
}
"#,
        );
        // One karac_par_run dispatch, three branch fns.
        let calls = ir.matches("call void @karac_par_run").count();
        assert_eq!(
            calls, 1,
            "expected one karac_par_run dispatch for parallax-lite shape; IR:\n{ir}"
        );
        for i in 0..3 {
            let needle = format!("__par_branch_0_{i}");
            assert!(
                ir.contains(&needle),
                "expected branch fn {needle} in IR:\n{ir}"
            );
        }
        // Empty slot list: no return struct synthesized.
        assert!(
            !ir.contains("__karac_ParGroup_0_Returns"),
            "no return struct should be emitted for the empty-slot path; IR:\n{ir}"
        );
        // No slot GEPs.
        assert!(
            !ir.contains("__par_slot_"),
            "no slot pointer/load instructions should appear; IR:\n{ir}"
        );
    }

    /// Regression: `let a = fetch_a(); let b = fetch_b(); use_v(a);
    /// use_v(b);` previously codegen'd with `Undefined variable 'b'`.
    ///
    /// Root cause was upstream of Slice A's slot ABI: the analyzer's
    /// greedy grouping in `find_parallel_groups` would skip over
    /// dependent middle stmts and emit non-contiguous parallel groups
    /// (e.g., `[0, 3]` and `[1, 2]`). The codegen's
    /// `i = max_idx + 1` step then jumped past the second group's
    /// stmts entirely, so `let b` never ran in the parent scope and
    /// `use_v(b)` (which had been pulled into the first group's
    /// branch fn for stmt 3) failed to resolve `b`.
    ///
    /// Fixed at the analyzer level — parallel groups are now
    /// contiguous-only (`src/concurrency.rs::find_parallel_groups`
    /// breaks on the first non-eligible candidate instead of
    /// skipping). This test pins that the source compiles cleanly
    /// without the spurious diagnostic.
    #[test]
    fn test_auto_par_non_contiguous_group_no_undefined_var() {
        let src = r#"
effect resource R1;
effect resource R2;

fn fetch_a() -> Vec[i64] reads(R1) {
    let mut v: Vec[i64] = Vec.new();
    v.push(42);
    v
}

fn fetch_b() -> Vec[i64] reads(R2) {
    let mut v: Vec[i64] = Vec.new();
    v.push(99);
    v
}

fn use_v(v: Vec[i64]) { println(v.len()); }

fn main() {
    let a: Vec[i64] = fetch_a();
    let b: Vec[i64] = fetch_b();
    use_v(a);
    use_v(b);
}
"#;
        use karac::codegen::compile_to_ir_with_options;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let res = compile_to_ir_with_options(&parsed.program, None, Some(&analysis), None, None);
        assert!(
            res.is_ok(),
            "expected clean compile post-contiguous-only fix; got: {:?}",
            res.err()
        );
    }

    /// Bug fix: when an auto-par group's branch-stmt body reads
    /// `self.X` (a `FieldAccess { object: SelfValue, ... }` shape),
    /// the capture-set computation in `emit_par_run` previously
    /// missed `self` because `refs_in_expr` had no `SelfValue` arm.
    /// The branch fn was emitted without `self` in its env-struct,
    /// and `load_variable("self")` inside the branch body errored
    /// with `Undefined variable 'self'`.
    ///
    /// Fixed 2026-05-09 in `src/codegen.rs::refs_in_expr`: added a
    /// `SelfValue` arm that inserts `"self"` into the refs set, so
    /// captures pick it up and the env-struct unpack rebinds `self`
    /// in the branch fn's `self.variables`. Surfaced when growing
    /// `examples/parallax/`'s `fetch_latest_order` from `-> Order`
    /// to `-> Vec[Order]`; the parallax demo also exposes a
    /// downstream slot-rebind use-after-free issue (tracked in
    /// `bugs.md`) so this regression test uses a smaller shape that
    /// only exercises the capture-set path.
    /// Regression for bug #3 (closed 2026-05-09): auto-par grouping
    /// puts a `Vec[T]` return through a slot, then the value is
    /// moved into the surrounding fn's returned struct. Pre-fix, the
    /// slot rebind unconditionally called `track_vec_var(alloca)`,
    /// scheduling a free at the parent's scope exit. When the slot
    /// value was moved into a returned struct, the free corrupted
    /// the moved Vec's data pointer → SIGABRT.
    ///
    /// Fixed in `src/codegen.rs::compile_function_body` slot rebind
    /// path: the `track_vec_var(alloca)` call was removed (see the
    /// inline comment for the leak/correctness trade-off rationale).
    /// This test pins the demo-shape end-to-end behaviour.
    #[test]
    fn test_auto_par_vec_slot_into_struct_returned() {
        // Build the binary and run it. Both println markers must
        // appear — if "after" is missing, the cleanup of the returned
        // struct's Vec fields is unsafe (use-after-free or double-free).
        let src = r#"
struct Holder {
    a: Vec[i64],
    b: Vec[i64],
}

trait DbA { fn fetch(ref self) -> Vec[i64]; }
trait DbB { fn fetch(ref self) -> Vec[i64]; }

pub effect resource R1: DbA;
pub effect resource R2: DbB;

struct InMemA {}
struct InMemB {}

impl DbA for InMemA {
    fn fetch(ref self) -> Vec[i64] {
        let mut v: Vec[i64] = Vec.new();
        v.push(1);
        v
    }
}

impl DbB for InMemB {
    fn fetch(ref self) -> Vec[i64] {
        let mut v: Vec[i64] = Vec.new();
        v.push(2);
        v
    }
}

fn fetch_a() -> Vec[i64] with reads(R1) { R1.fetch() }
fn fetch_b() -> Vec[i64] with reads(R2) { R2.fetch() }

fn assemble() -> Holder with reads(R1) reads(R2) {
    let a = fetch_a();
    let b = fetch_b();
    Holder { a: a, b: b }
}

fn main() {
    let mut da = InMemA {};
    let mut db = InMemB {};
    with_provider[R1](da, || {
        with_provider[R2](db, || {
            let h = assemble();
            println("before");
            let _ = h;
            println("after");
        });
    });
}
"#;
        // Inline the link-and-run helper from tests/parallax.rs.
        use karac::codegen::{compile_to_object_with_options, link_executable};
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let obj = format!("/tmp/karac_bug3_{pid}_{id}.o");
        let exe = format!("/tmp/karac_bug3_{pid}_{id}");
        compile_to_object_with_options(
            &parsed.program,
            &obj,
            Some(&ownership),
            Some(&analysis),
            None,
            None,
        )
        .unwrap();
        link_executable(&obj, &exe).unwrap();
        let out = super::common::output_with_hang_watchdog(
            std::process::Command::new(&exe),
            std::time::Duration::from_secs(60),
        )
        .expect("child binary spawn failed");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&exe);
        assert!(
            out.status.success(),
            "binary should exit cleanly; got {:?}, stdout:\n{stdout}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            stdout.trim(),
            "before\nafter",
            "expected both markers; if `after` is missing, the \
             cleanup of the returned Holder (whose fields came \
             through par-slot rebinds) corrupted memory."
        );
    }

    #[test]
    fn test_impl_ref_self_self_field_in_branch_stmt_captures_self() {
        let src = r#"
struct InMemOrders { seed: i64 }

impl InMemOrders {
    fn get_orders(ref self, user_id: i64) -> Vec[i64] {
        let s: i64 = self.seed + user_id;
        let mut v: Vec[i64] = Vec.new();
        v.push(s);
        v
    }
}

fn main() {
    let db = InMemOrders { seed: 100 };
    let _ = db.get_orders(42);
    println("done");
}
"#;
        use karac::codegen::compile_to_ir_with_options;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let res = compile_to_ir_with_options(&parsed.program, None, Some(&analysis), None, None);
        assert!(
            res.is_ok(),
            "expected clean compile (SelfValue capture-set fix); got: {:?}",
            res.err()
        );
    }

    // ── Auto-par reduction lowering (slice 3a wiring, 2026-05-19) ─────
    //
    // Slice 3a only declares the `karac_par_reduce` extern in the codegen
    // module; the actual lowering of recognized reductions into a
    // fan-out + serial-combine call lands in slice 3b. The test below
    // pins that the extern is declared in every emitted module — so
    // slice 3b's first commit can call into it without adding the extern
    // in the same diff.

    #[test]
    fn test_ir_declares_karac_par_reduce_extern_slice3a() {
        // The simplest program possible — no reductions, no par blocks.
        // The extern is declared unconditionally at codegen init so the
        // symbol is linkable from any module the karac compiler emits,
        // not just modules that happen to call it.
        let ir = ir_for(r#"fn main() { println(0); }"#);
        assert!(
            ir.contains("declare void @karac_par_reduce"),
            "IR should declare karac_par_reduce as an extern (slice 3a wiring); got:\n{ir}"
        );
    }

    /// Slice 3b — pipeline-IR test: a `for k in 0..N { acc = acc + k }`
    /// reduction lowers to a `call void @karac_par_reduce` site with a
    /// synthesized worker fn. Uses the lowered pipeline (resolve +
    /// typecheck + lower + effectcheck + concurrency) so the analyzer's
    /// `LoopReduction` tag is threaded through to codegen.
    #[test]
    fn test_ir_reduction_emits_par_reduce_call_slice3b() {
        // K = 100_000 puts total cost at ~300_000 unit-iters (sum + add
        // per iter), well above the slice-3b.5 cost-model threshold of
        // 80_000 unit-iters. Without the bump from K=1000 to K=100_000,
        // the gate would block this loop from lowering — which would be
        // correct behavior but defeats the test's purpose of pinning
        // the IR-emission shape. The dedicated gate test below uses
        // K=100 to pin the small-loop-skips-lowering behavior.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..100000i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "expected a karac_par_reduce call site; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_worker_"),
            "expected a synthesized reduce worker fn; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_init_add_i64"),
            "expected the Add+i64 init helper; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_add_i64"),
            "expected the Add+i64 combine helper; got:\n{ir}"
        );
    }

    /// B-2026-06-15-3: a reduction that captures a large fixed-size array
    /// must pass it through the worker env BY POINTER, not inline. Capturing
    /// `[N x i64]` by value made the worker `alloca [N x i64]` + load the
    /// whole env + `store [N x i64]` (an N*8-byte copy); LLVM's O2 scalarized
    /// that into N element load/stores, then DAGCombiner store-merging +
    /// alias analysis went super-linear — auto-par compile of brute_force was
    /// 0.07 -> 2.15 s (30x) and #3629's bfs_sieve hit 60 s / 639 MiB. The fix
    /// stores the parent array's pointer in the env (a read-only reduction
    /// input; the parent frame outlives the join). Pin it: the worker must NOT
    /// `alloca`/`store` the array by value (GEP-through-the-pointer access,
    /// which still names `[N x i64]`, is fine).
    #[test]
    fn test_reduce_worker_captures_array_by_pointer_not_inline() {
        let src = r#"
fn inner(data: Array[i64, 2048]) -> i64 {
    let mut s: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < 2048i64 {
        s = s + data[i];
        i = i + 1i64;
    }
    return s;
}
fn run(data: Array[i64, 2048]) -> i64 {
    let mut acc: i64 = 0i64;
    for _ in 0i64..100i64 {
        acc = acc + inner(data);
    }
    return acc;
}
fn main() {
    let data: Array[i64, 2048] = [1i64; 2048];
    println(run(data));
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        let worker = ir
            .split("@__karac_reduce_worker_")
            .nth(1)
            .and_then(|s| s.split("\n}").next())
            .unwrap_or("");
        assert!(
            !worker.is_empty(),
            "expected a synthesized reduce worker fn; IR:\n{ir}"
        );
        assert!(
            !worker.contains("alloca [2048 x i64]") && !worker.contains("store [2048 x i64]"),
            "reduce worker must capture the array BY POINTER — no alloca/store \
             of the [2048 x i64] by value (the O2 store-merge compile blowup, \
             B-2026-06-15-3). Worker IR:\n{worker}"
        );
    }

    /// Follow-on to B-2026-06-15-3 (the by-pointer array capture above): a
    /// reduction body that indexes a captured fixed-size array by a modulo of
    /// the loop var (`let idx = k % m; ... atab[idx]`) must NOT run the
    /// modulo-BCE preflight against the array. That preflight is a Vec-only
    /// optimization — it reads `{ptr,len,cap}` field 1 as the runtime length —
    /// but a by-pointer array capture has no header, so `build_struct_gep(_, _,
    /// 1)` reads element[1] as the "length" and can trap on that garbage.
    /// Kata 60 hit it: `btab = [1,2,3,4]` → element[1] = 2 < upper 4 → the
    /// preflight panicked `vec index out of bounds` under the default auto-par
    /// build while the seq lane was fine. An array's length is a compile-time
    /// constant N, so its per-iter `[N x T]` bounds check is already correct;
    /// the recognizer must simply skip array captures. Pin: the worker holds
    /// the correct per-iter array check (`getelementptr [4 x i64]`) and NO
    /// Vec-header read of the array pointer / `vec index out of bounds` panic.
    #[test]
    fn test_reduce_captured_array_modulo_index_no_vec_bce_preflight() {
        let src = r#"
fn f(a: i64, b: i64) -> i64 { a * 100i64 + b }
fn main() {
    let m: i64 = 4i64;
    let atab: Array[i64, 4] = [10i64, 20i64, 30i64, 40i64];
    let btab: Array[i64, 4] = [1i64, 2i64, 3i64, 4i64];
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        let idx = k % m;
        total = total + f(atab[idx], btab[idx]);
        k = k + 1i64;
    }
    println(total);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        let worker = ir
            .split("define void @__karac_reduce_worker_")
            .nth(1)
            .and_then(|s| s.split("\n}").next())
            .unwrap_or("");
        assert!(
            !worker.is_empty(),
            "expected a synthesized reduce worker fn; IR:\n{ir}"
        );
        // No Vec-header preflight against the array capture, and no vec-OOB
        // panic path — only the correct per-iter array GEP/bounds check.
        assert!(
            !worker.contains("bce.len"),
            "reduce worker must NOT emit the modulo-BCE Vec-header preflight for \
             a fixed-size array capture (reads element[1] as a bogus length). \
             Worker IR:\n{worker}"
        );
        assert!(
            !worker.contains("vec index out of bounds"),
            "array capture must not route through the Vec index-OOB panic; \
             the per-iter check is the static-N array check. Worker IR:\n{worker}"
        );
        assert!(
            worker.contains("getelementptr [4 x i64]"),
            "expected the correct per-iter [4 x i64] array GEP. Worker IR:\n{worker}"
        );
    }

    /// End-to-end twin of the pin above: the captured-array modulo-index
    /// reduction compiles, links, and runs under the default auto-par build
    /// (which previously panicked `vec index out of bounds` at the preflight),
    /// producing the correct sum. `atab` cycles 10,20,30,40 and `btab` cycles
    /// 1,2,3,4 over k∈[0,100000): 25000 full cycles, per-cycle Σ f = Σ(a·100+b)
    /// = (100·100 + 10) = 10010, so total = 25000 · 10010 = 250250000.
    #[test]
    fn test_e2e_reduction_captured_array_modulo_index_matches_serial() {
        let src = r#"
fn f(a: i64, b: i64) -> i64 { a * 100i64 + b }
fn main() {
    let m: i64 = 4i64;
    let atab: Array[i64, 4] = [10i64, 20i64, 30i64, 40i64];
    let btab: Array[i64, 4] = [1i64, 2i64, 3i64, 4i64];
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        let idx = k % m;
        total = total + f(atab[idx], btab[idx]);
        k = k + 1i64;
    }
    println(total);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "250250000");
    }

    /// Slice 3b end-to-end: compile + link + run a program with a
    /// recognized reduction loop and verify the output matches the
    /// serial Σ-formula. Pinning correctness against the parallel
    /// dispatch — the recognizer surfaced the loop, codegen lowered it
    /// to `karac_par_reduce`, the runtime fanned it out across workers,
    /// the slot-combine produced the right answer.
    #[test]
    fn test_e2e_reduction_for_range_add_i64_matches_serial_slice3b() {
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..1000i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [0, 1000) = 999 * 1000 / 2 = 499500
        assert_eq!(out.trim(), "499500");
    }

    /// Slice 3b end-to-end: a larger N to exercise the multi-worker
    /// dispatch path (smaller-N tests hit the single-worker fast path
    /// inside `karac_par_reduce`). 100K iters land well above any pool
    /// size, so chunks land on every available worker thread.
    #[test]
    fn test_e2e_reduction_for_range_add_i64_multi_worker_slice3b() {
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..100000i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [0, 100000) = (100000 * 99999) / 2 = 4999950000
        assert_eq!(out.trim(), "4999950000");
    }

    #[test]
    fn test_e2e_recursive_reduction_nqueens_count_bounded_by_fork_depth_cap() {
        // A backtracking counter whose `+` reduction delta RECURSES into its
        // own function (`total = total + count(...deeper...)`). Before the
        // runtime fork-depth cap this SIGBUS'd under auto-par — each recursion
        // level fanned out a nested parallel region and the nesting exhausted
        // the stack (B-2026-07-03-14). The cap makes only the outermost level
        // parallelize; deeper levels run inline, so it completes correctly.
        // n = 9 N-Queens has 352 solutions. Regression for the shallow-depth
        // parallel-reduction slice.
        let src = r#"
fn count(n: i64, row: i64, cols: i64, diag1: i64, diag2: i64) -> i64 {
    if row == n { return 1i64; }
    let mut total = 0i64;
    let mut c = 0i64;
    while c < n {
        let bit_c = 1i64 << c;
        let bit_d1 = 1i64 << (row + c);
        let bit_d2 = 1i64 << (row - c + (n - 1i64));
        if (cols & bit_c) == 0i64 and (diag1 & bit_d1) == 0i64 and (diag2 & bit_d2) == 0i64 {
            total = total + count(n, row + 1i64, cols | bit_c, diag1 | bit_d1, diag2 | bit_d2);
        }
        c = c + 1i64;
    }
    total
}
fn main() { println(count(9i64, 0i64, 0i64, 0i64, 0i64)); }
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "352");
    }

    // ── Slice 3b.4: while-shape support ──────────────────────────────
    //
    // The kata-7 bench (and many real workloads) write the K-iter loop
    // as `let mut k = 0; while k < K { ...; k = k + 1; }` instead of
    // `for k in 0..K`. The recognizer (slice 1) already accepts both
    // shapes — slice 3b.4 teaches codegen to lower the while shape too.
    // Same fan-out + serial-combine machinery, just an extra shape
    // extractor that strips the body's terminal `k = k + 1` increment
    // and verifies the preceding `let mut k: T = 0;` init.

    #[test]
    fn test_ir_reduction_while_shape_emits_par_reduce_call_slice3b4() {
        // K = 100_000 to cross the slice-3b.5 cost-model gate threshold
        // (see test_ir_reduction_emits_par_reduce_call_slice3b for the
        // same rationale; the dedicated gate test uses K=100 to pin the
        // small-loop-skips-lowering behavior).
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "while-shape reduction should lower to karac_par_reduce; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_worker_"),
            "expected synthesized worker fn; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_while_shape_matches_serial_slice3b4() {
        // Same K, same Σ formula as the for-loop test — verifies the
        // while-shape path produces the same value as the for-loop path
        // and as the serial Σ formula.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 1000i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "499500");
    }

    #[test]
    fn test_e2e_reduction_while_shape_multi_worker_slice3b4() {
        // Larger N to hit the multi-worker dispatch path.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "4999950000");
    }

    // ── Slice 3b.5: cost-model gate ──────────────────────────────────
    //
    // When the iteration count is statically known and total work is
    // below the dispatch threshold (~80,000 unit-iterations ≈ ~80µs),
    // the lowering bails and sequential codegen runs. Tests pin both
    // directions: small K → no par_reduce call; large K → par_reduce
    // call. The dispatch-overhead-vs-work calibration lives in
    // `src/codegen/reduce.rs`'s `REDUCE_DISPATCH_THRESHOLD_UNITS`.

    #[test]
    fn test_ir_cost_gate_blocks_small_loop_slice3b5() {
        // K = 100 with body cost ~3 (sum + add per iter) → 300 unit-iters,
        // far below the 80,000 threshold. Expected: no par_reduce call
        // in the IR; sequential for-loop codegen runs instead. Sink is
        // still correct (just not parallelized).
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..100i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            !ir.contains("call void @karac_par_reduce"),
            "small loop (K=100, trivial body) should NOT lower to karac_par_reduce; \
             cost-model gate should block. Got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_cost_gate_small_loop_still_correct_slice3b5() {
        // Same small-K case as the IR test, plus an E2E correctness
        // check: even though the loop runs sequentially, the sink must
        // match the Σ formula. Pins that the cost-model gate is a
        // codegen optimization (skips lowering), not a semantic change.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..100i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [0, 100) = 99 * 100 / 2 = 4950
        assert_eq!(out.trim(), "4950");
    }

    #[test]
    fn test_ir_cost_gate_blocks_small_loop_with_method_call_slice3b5() {
        // K = 100 with a method-call body (cost ≈ 11 per iter, including
        // CALL_COST_UNITS = 10). Total ~1100 unit-iters, still well below
        // the 80,000 threshold. Pins that the cost gate sees through the
        // function-call weight and still bails.
        let src = r#"
fn double(x: i64) -> i64 { x + x }

fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..100i64 {
        sum = sum + double(k);
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            !ir.contains("call void @karac_par_reduce"),
            "K=100 with method-call body still below threshold; gate should block. Got:\n{ir}"
        );
    }

    #[test]
    fn test_ir_cost_gate_allows_variable_k_slice3b5() {
        // Variable-K loops bypass the compile-time gate (K isn't a
        // literal at codegen time). Even with a trivial body, the
        // lowering fires because the runtime can't see through to
        // const-eval k_iters cheaply. In practice variable-K loops are
        // typically large (kata-7 = 50M). A dynamic runtime-side gate
        // is a follow-up; the v1 cost-model is compile-time-only.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k_iters: i64 = 100i64;
    let mut k: i64 = 0i64;
    while k < k_iters {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "variable-K loop should bypass the compile-time gate and lower; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_while_compound_assign_increment_slice3b4() {
        // The `k += 1` shape parses to CompoundAssign, not Assign. Both
        // forms route through `is_step_one_increment_stmt`; this test
        // pins that the compound-assign branch lands too.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 1000i64 {
        sum = sum + k;
        k += 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "499500");
    }

    // ── Slice 3b.1: non-Add ops (Mul / BitOr / BitAnd / BitXor) ──────
    //
    // The recognizer (slice 1) tags any of the five associative +
    // commutative ops as reductions, but slice 3b's narrow lowering only
    // handled `+`. These tests pin that each remaining op lowers to a
    // (op, type)-named helper pair (`__karac_reduce_init_<op>_<ty>` /
    // `__karac_reduce_combine_<op>_<ty>`) and produces the correct fold
    // under multi-worker dispatch. Variable-K loops (`for k in 0..k_iters`
    // where `k_iters` is an outer-scope local) bypass the slice-3b.5
    // cost-model gate, so small K can be used to keep test computations
    // within i64 range while still exercising the lowering path.

    #[test]
    fn test_ir_reduction_mul_i64_slice3b1() {
        let src = r#"
fn main() {
    let k_iters: i64 = 18i64;
    let mut prod: i64 = 1i64;
    for k in 0i64..k_iters {
        prod = prod * (k + 1i64);
    }
    println(prod);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("@__karac_reduce_init_mul_i64"),
            "expected Mul+i64 init helper; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_mul_i64"),
            "expected Mul+i64 combine helper; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_mul_i64_slice3b1() {
        // 18! = 6_402_373_705_728_000 fits in i64 (9_223_372_036_854_775_807);
        // 19! overflows. Identity for Mul is 1, so per-worker partials all
        // start at 1 — if the init helper accidentally wrote 0 the combine
        // fold would produce 0, which this assertion catches.
        let src = r#"
fn main() {
    let k_iters: i64 = 18i64;
    let mut prod: i64 = 1i64;
    for k in 0i64..k_iters {
        prod = prod * (k + 1i64);
    }
    println(prod);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "6402373705728000");
    }

    #[test]
    fn test_ir_reduction_bitor_i64_slice3b1() {
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut acc: i64 = 0i64;
    for k in 0i64..k_iters {
        acc = acc | k;
    }
    println(acc);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("@__karac_reduce_init_bitor_i64"),
            "expected BitOr+i64 init helper; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_bitor_i64"),
            "expected BitOr+i64 combine helper; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_bitor_i64_slice3b1() {
        // OR of [0, 100): all bits up to and including bit 6 set = 127
        // (next power of 2 >= 100 is 128 = 2^7).
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut acc: i64 = 0i64;
    for k in 0i64..k_iters {
        acc = acc | k;
    }
    println(acc);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "127");
    }

    #[test]
    fn test_e2e_reduction_bitor_i64_multi_worker_slice3b1() {
        // K = 100_000 forces multi-chunk dispatch (each worker handles
        // ~K/N iters); OR over [0, 100_000) = 131_071 (next power of 2
        // >= 100_000 is 131_072 = 2^17). Per-worker partials each cover
        // a sub-range; the OR-combine fold over them is the OR over
        // [0, 100_000) — same answer as the serial loop.
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut acc: i64 = 0i64;
    for k in 0i64..k_iters {
        acc = acc | k;
    }
    println(acc);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "131071");
    }

    #[test]
    fn test_ir_reduction_bitand_i64_slice3b1() {
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut acc: i64 = -1i64;
    for k in 0i64..k_iters {
        acc = acc & 255i64;
    }
    println(acc);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("@__karac_reduce_init_bitand_i64"),
            "expected BitAnd+i64 init helper; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_bitand_i64"),
            "expected BitAnd+i64 combine helper; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_bitand_i64_slice3b1() {
        // BitAnd identity is all-ones (-1). Per-worker acc starts at -1
        // and folds `acc & 255` → 255 after iteration 1, stays at 255.
        // Combine via AND of 255 with itself across workers stays 255.
        // If the init helper wrote 0 instead of all-ones, the per-worker
        // partials would be 0 and the final fold would be 0 — caught
        // by this assertion.
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut acc: i64 = -1i64;
    for k in 0i64..k_iters {
        acc = acc & 255i64;
    }
    println(acc);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "255");
    }

    #[test]
    fn test_ir_reduction_bitxor_i64_slice3b1() {
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut acc: i64 = 0i64;
    for k in 0i64..k_iters {
        acc = acc ^ k;
    }
    println(acc);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("@__karac_reduce_init_bitxor_i64"),
            "expected BitXor+i64 init helper; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_bitxor_i64"),
            "expected BitXor+i64 combine helper; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_bitxor_i64_slice3b1() {
        // XOR of [0, N) cycles with period 4:
        //   N % 4 == 0  ->  0
        //   N % 4 == 1  ->  N-1
        //   N % 4 == 2  ->  1
        //   N % 4 == 3  ->  N
        // For N = 100 (mod 4 == 0): result is 0.
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut acc: i64 = 0i64;
    for k in 0i64..k_iters {
        acc = acc ^ k;
    }
    println(acc);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "0");
    }

    // ── Slice 3b.2: non-i64 accumulator types ────────────────────────
    //
    // Generalizes the worker fn + descriptor to any int width LLVM
    // exposes (i8 / i16 / i32 / i64). The runtime ABI keeps i64
    // start/end params on workers (unchanged across this slice); the
    // worker fn truncates them to the source-level loop var type on
    // entry so the body's `acc <op> k` lowers with matching int types.

    #[test]
    fn test_ir_reduction_add_i32_slice3b2() {
        let src = r#"
fn main() {
    let k_iters: i32 = 1000i32;
    let mut sum: i32 = 0i32;
    for k in 0i32..k_iters {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("@__karac_reduce_init_add_i32"),
            "expected Add+i32 init helper (separate symbol from Add+i64); got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_add_i32"),
            "expected Add+i32 combine helper; got:\n{ir}"
        );
        // For non-i64 accumulators, the worker fn truncates i64 start/end
        // (the runtime ABI types) to acc_int_ty in its prologue. Pin
        // those instructions so a regression that drops the truncation
        // (and produces a type-mismatched cmp / store) is caught at the
        // IR layer rather than crashing the e2e run.
        assert!(
            ir.contains("start.trunc"),
            "expected i64 start to be truncated to i32 in worker fn; got:\n{ir}"
        );
        assert!(
            ir.contains("end.trunc"),
            "expected i64 end to be truncated to i32 in worker fn; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_add_i32_slice3b2() {
        // Σ k for k in [0, 1000) = 999 * 1000 / 2 = 499500. Fits in
        // i32 (max 2_147_483_647). Multi-worker exercised since K > N
        // for any reasonable worker count.
        let src = r#"
fn main() {
    let k_iters: i32 = 1000i32;
    let mut sum: i32 = 0i32;
    for k in 0i32..k_iters {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "499500");
    }

    // ── Slice 3b.3: non-zero `lo` in for-range ───────────────────────
    //
    // Threads the source-level start bound `lo` through env-struct
    // field 0 so the worker can recover the real iteration value
    // `k = lo + worker_local_index`. Lifts the slice-3b/3b.1/3b.2 gate
    // that rejected any for-range whose start wasn't `0` / absent.
    // While-shape continues to require zero init (`let mut k: T = 0`);
    // a sibling slice could extend it.

    #[test]
    fn test_ir_reduction_for_range_non_zero_lo_slice3b3() {
        // Variable lo so the parent's `end - lo` doesn't constant-fold
        // away — pins the named `iter.total` sub instruction. Variable-K
        // also bypasses the slice-3b.5 cost-model gate so the lowering
        // fires regardless of K.
        let src = r#"
fn main() {
    let lo: i64 = 5i64;
    let mut sum: i64 = 0i64;
    for k in lo..1000i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "expected a karac_par_reduce call site for non-zero lo; got:\n{ir}"
        );
        // Pin the env-struct shape — lo lives at field 0 even when
        // there are no source-level captures, so the env name appears
        // in the IR (lo_val.is_some() forces env_ctx_ptr to be non-null).
        assert!(
            ir.contains("__reduce_env"),
            "expected an env-struct alloca (lo threading needs ctx); got:\n{ir}"
        );
        assert!(
            ir.contains("__reduce_lo"),
            "expected lo unpack/insert symbol in worker fn; got:\n{ir}"
        );
        // Worker fn shifts chunk-local start/end by lo.
        assert!(
            ir.contains("start.shift") && ir.contains("end.shift"),
            "expected worker to shift both raw_start and raw_end by lo; got:\n{ir}"
        );
        // Parent computes `iter_total = end - lo`. Named instruction
        // present because at least one operand is a runtime value.
        assert!(
            ir.contains("iter.total"),
            "expected parent to compute iter_total = end - lo; got:\n{ir}"
        );
    }

    /// Multi-worker E2E: non-zero literal lo with K = 100_000 iters,
    /// guaranteed to dispatch chunks to multiple workers. Σ over the
    /// shifted range pins correctness through the lo-shift path in the
    /// worker.
    #[test]
    fn test_e2e_reduction_for_range_non_zero_lo_multi_worker_slice3b3() {
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    for k in 5i64..100005i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [5, 100005) = Σ k for k in [0, 100005)
        //   - Σ k for k in [0, 5)
        // = 100004 * 100005 / 2 - 4 * 5 / 2
        // = 5000450010 - 10
        // = 5000450000
        assert_eq!(out.trim(), "5000450000");
    }

    /// Variable-lo E2E (lo + hi both runtime bindings): bypasses the
    /// const-eval cost gate so the lowering fires for any K. Pins that
    /// captures of `lo`-as-runtime-value flow through the env-struct
    /// correctly — `lo` lands at field 0 and is added to the worker's
    /// chunk-local start/end, no separate identifier capture needed.
    #[test]
    fn test_e2e_reduction_for_range_variable_lo_slice3b3() {
        let src = r#"
fn main() {
    let lo: i64 = 10i64;
    let hi: i64 = 1010i64;
    let mut sum: i64 = 0i64;
    for k in lo..hi {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [10, 1010) = (1009 * 1010 / 2) - (9 * 10 / 2)
        // = 509545 - 45 = 509500
        assert_eq!(out.trim(), "509500");
    }

    /// Non-Add op + non-zero lo: pins the (op, type, lo) matrix corner
    /// where multiple slice-3b.x generalizations interact. Mul reduction
    /// over [3, 10) = 3 * 4 * 5 * 6 * 7 * 8 * 9 = 181440. Cost-gate
    /// bypass via variable lo so the lowering fires despite low K.
    #[test]
    fn test_e2e_reduction_mul_non_zero_lo_slice3b3() {
        let src = r#"
fn main() {
    let lo: i64 = 3i64;
    let mut prod: i64 = 1i64;
    for k in lo..10i64 {
        prod = prod * k;
    }
    println(prod);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "181440");
    }

    // ── Slice 3b.8: runtime-side dynamic cost gate ───────────────────
    //
    // Codegen-time gate (slice 3b.5) only catches loops whose iteration
    // count is a literal at compile time. Variable-K loops bypass and
    // always emit a `karac_par_reduce` call. Slice 3b.8 plumbs a per-
    // iter cost estimate through the descriptor so the runtime can
    // short-circuit to a sequential single-worker invocation when the
    // estimated total work falls below the dispatch threshold. The IR
    // test here pins the descriptor's new field (`d.per_iter_cost`
    // insertvalue at index 7); runtime gate behavior is exercised by
    // the AtomicUsize-counter tests in `runtime/src/lib.rs`.

    #[test]
    fn test_ir_reduction_descriptor_emits_per_iter_cost_units_slice3b8() {
        // K = 100_000 keeps the codegen-time gate happy (literal K, real
        // body cost above threshold) so the descriptor is actually
        // emitted. The IR check below pins the descriptor struct shape
        // (8 fields, with `i64` last for `per_iter_cost_units`); LLVM
        // folds the insertvalue chain into a constant aggregate when
        // every field is a constant, so the named `d.per_iter_cost`
        // instruction doesn't survive to the printed IR — the struct
        // type signature is the stable IR-level pin.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..100000i64 {
        sum = sum + k;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        // Pin the descriptor's 8-field shape — slice 3b emitted 7 (i64
        // iter_total + i64 slot_size + i64 slot_align + ptr init + ptr
        // worker + ptr combine + ptr ctx); slice 3b.8 appends i64
        // per_iter_cost_units so the type signature gains one trailing
        // i64.
        assert!(
            ir.contains("{ i64, i64, i64, ptr, ptr, ptr, ptr, i64 }"),
            "expected 8-field reduce descriptor type with trailing i64 per_iter_cost_units; \
             got:\n{ir}"
        );
        // Body cost for `sum = sum + k` walks to 2 units (Assign = 1
        // base + Binary{Add} = 1; identifier loads are 0). The doc
        // bumps that to 11 — let's just assert the trailing constant is
        // a small positive number rather than 0 (so a future cost-
        // estimator tweak doesn't break the test) by spot-checking that
        // the trailing field of the constant aggregate isn't `i64 0`.
        assert!(
            !ir.contains("ptr null, i64 0 }"),
            "expected non-zero per_iter_cost_units in the const aggregate (0 is the runtime \
             sentinel = 'always dispatch'); got:\n{ir}"
        );
    }

    // ── Slice 3b.9: while-shape non-zero literal init ────────────────
    //
    // Mirror of slice 3b.3 for the while-loop path. Today's
    // preceding_stmt_int_literal_init returns Some(init_expr) for any
    // int literal; the extract_loop_shape While arm normalizes Integer(0)
    // → lo_expr = None (current path) and any other int literal →
    // lo_expr = Some(expr). All the lo-shift machinery from 3b.3
    // (env-struct field 0, worker fn start/end add) is reused unchanged.
    // Variable init (e.g. `let mut k: T = lo;`) defers to a later slice.

    #[test]
    fn test_ir_reduction_while_shape_non_zero_init_slice3b9() {
        // K = 100_000 puts the codegen-time cost gate above its threshold
        // so the descriptor is actually emitted. Body has the same shape
        // as the while-shape 3b.4 test (init + < + acc fold + k += 1)
        // with the init bumped from 0 to 5.
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 5i64;
    while k < 100005i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "expected a karac_par_reduce call site for non-zero init while-shape; got:\n{ir}"
        );
        // Env-struct must exist + carry lo at field 0 + worker shifts
        // raw_start/raw_end — same lo-threading IR shape as the for-
        // range non-zero lo test (slice 3b.3), proving the mirror.
        assert!(
            ir.contains("__reduce_env"),
            "expected env-struct alloca for init threading; got:\n{ir}"
        );
        assert!(
            ir.contains("__reduce_lo"),
            "expected lo unpack/insert symbol in worker fn; got:\n{ir}"
        );
        assert!(
            ir.contains("start.shift") && ir.contains("end.shift"),
            "expected worker to shift raw_start and raw_end by init; got:\n{ir}"
        );
    }

    /// Small-N E2E correctness for the while-shape non-zero init path.
    /// Pin against the slice 3b.5 cost gate by keeping K=100 (gated to
    /// sequential); even gated, lowering correctness should match the
    /// closed-form serial sum.
    #[test]
    fn test_e2e_reduction_while_shape_non_zero_init_small_n_slice3b9() {
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 3i64;
    while k < 103i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [3, 103) = (102 * 103 / 2) - (2 * 3 / 2) = 5253 - 3 = 5250
        assert_eq!(out.trim(), "5250");
    }

    /// Multi-worker E2E: K = 100_000 init=10 → dispatches across pool
    /// workers; the worker's chunk-local shift by init must agree across
    /// workers so the combine yields the source-level Σ.
    #[test]
    fn test_e2e_reduction_while_shape_non_zero_init_multi_worker_slice3b9() {
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 10i64;
    while k < 100010i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [10, 100010) = (100009 * 100010 / 2) - (9 * 10 / 2)
        // = 5000950045 - 45 = 5000950000
        assert_eq!(out.trim(), "5000950000");
    }

    // ── Slice 3b.10: while-shape arbitrary init expression ───────────
    //
    // Lifts slice 3b.9's int-literal-only init gate to accept any init
    // expression by synthesizing `lo_expr = Identifier(loop_var)`.
    // compile_expr loads from the parent's already-initialized k alloca,
    // so the init expression is evaluated once (by the let-stmt itself,
    // already compiled before we reach the while-stmt) regardless of
    // whether it has side effects. The "nothing modifies k between let
    // and while" invariant is guaranteed by the adjacent-stmt check in
    // preceding_stmt_init.

    #[test]
    fn test_ir_reduction_while_shape_variable_init_slice3b10() {
        // Variable lo bypasses the cost-model gate so the lowering
        // fires regardless of K — easier to pin the IR shape.
        let src = r#"
fn main() {
    let lo: i64 = 7i64;
    let mut sum: i64 = 0i64;
    let mut k: i64 = lo;
    while k < 1000i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        // Same lo-threading shape as slice 3b.9 — the par_reduce call
        // exists, env-struct + __reduce_lo + start.shift / end.shift
        // markers are all present. The new bit is that the loaded
        // value comes from the parent's k alloca (load instruction in
        // the par_reduce setup block), not a constant.
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "expected a karac_par_reduce call site for variable-init while-shape; got:\n{ir}"
        );
        assert!(
            ir.contains("__reduce_lo"),
            "expected lo unpack/insert symbol; got:\n{ir}"
        );
        assert!(
            ir.contains("start.shift") && ir.contains("end.shift"),
            "expected worker to shift raw_start/raw_end by loaded k; got:\n{ir}"
        );
    }

    /// Multi-worker E2E: variable init via outer `let lo` + multi-worker
    /// K to exercise the chunked dispatch path with a loaded shift value.
    #[test]
    fn test_e2e_reduction_while_shape_variable_init_multi_worker_slice3b10() {
        let src = r#"
fn main() {
    let lo: i64 = 20i64;
    let mut sum: i64 = 0i64;
    let mut k: i64 = lo;
    while k < 100020i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [20, 100020) = (100019 * 100020 / 2) - (19 * 20 / 2)
        // = 5001950190 - 190 = 5001950000
        assert_eq!(out.trim(), "5001950000");
    }

    /// Complex init expression — `let mut k: i64 = base + offset;` —
    /// confirms the load-from-k-alloca path handles any init RHS, not
    /// just simple identifiers. The let-stmt evaluates `base + offset`
    /// once; the par_reduce setup loads the result from k.
    #[test]
    fn test_e2e_reduction_while_shape_complex_init_slice3b10() {
        let src = r#"
fn main() {
    let base: i64 = 30i64;
    let offset: i64 = 7i64;
    let mut sum: i64 = 0i64;
    let mut k: i64 = base + offset;
    while k < 100037i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ k for k in [37, 100037) = (100036 * 100037 / 2) - (36 * 37 / 2)
        // = 5003650666 - 666 = 5003650000
        assert_eq!(out.trim(), "5003650000");
    }

    // ── Slice: Min/Max combined (2026-05-20) ─────────────────────────
    //
    // Min/Max recognition + codegen lowering — analyzer detects both the
    // direct call form (`acc = T.min(acc, x)`) and the conditional-assign
    // form (`if x < acc { acc = x; }`); codegen emits identity via
    // `signed_int_max`/`signed_int_min` (i64::MAX / i64::MIN at the
    // accumulator's int type) and combine via `icmp slt`+`select` for
    // Min, `icmp sgt`+`select` for Max. Validation workload:
    // kara-katas/leetcode/101-200/153-find-minimum-in-rotated-sorted-array.

    #[test]
    fn test_ir_reduction_min_conditional_assign() {
        // Conditional-assign Min shape — variable-K loop bypasses the
        // cost-model gate, so the helper-symbol emission path is
        // exercised regardless of K. IR pins:
        //   - `__karac_reduce_init_min_i64` (identity = i64::MAX)
        //   - `__karac_reduce_combine_min_i64`
        //   - `icmp slt` + `select` inside the combine body
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut m: i64 = 1000i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let x: i64 = i * 7i64;
        if x < m {
            m = x;
        }
        i = i + 1i64;
    }
    println(m);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("@__karac_reduce_init_min_i64"),
            "expected Min+i64 init helper; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_min_i64"),
            "expected Min+i64 combine helper; got:\n{ir}"
        );
        // The combine body uses `icmp slt` + `select` — pin both so a
        // future refactor to e.g. `llvm.smin.i64` intrinsics flags the
        // change explicitly. The InstCombine pass at `-O2` lifts the
        // select+icmp idiom to the intrinsic at the backend layer, but
        // we emit the source-of-truth IR at the karac layer.
        assert!(
            ir.contains("icmp slt"),
            "expected icmp slt in Min combine; got:\n{ir}"
        );
        assert!(
            ir.contains("%min ="),
            "expected select named %min in combine; got:\n{ir}"
        );
    }

    #[test]
    fn test_ir_reduction_max_conditional_assign() {
        // Mirror of the Min IR test for Max: identity = i64::MIN,
        // combine = `icmp sgt` + `select`.
        let src = r#"
fn main() {
    let k_iters: i64 = 100i64;
    let mut m: i64 = -1000i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let x: i64 = i * 7i64;
        if x > m {
            m = x;
        }
        i = i + 1i64;
    }
    println(m);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("@__karac_reduce_init_max_i64"),
            "expected Max+i64 init helper; got:\n{ir}"
        );
        assert!(
            ir.contains("@__karac_reduce_combine_max_i64"),
            "expected Max+i64 combine helper; got:\n{ir}"
        );
        assert!(
            ir.contains("icmp sgt"),
            "expected icmp sgt in Max combine; got:\n{ir}"
        );
        assert!(
            ir.contains("%max ="),
            "expected select named %max in combine; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_min_conditional_assign_multi_worker() {
        // K = 100_000 forces multi-chunk dispatch. The series
        // `(i * 7 + 11) % 977` for i in [0, 100_000) hits its minimum at
        // many points; the actual minimum value over the range is 0
        // (when `(7i + 11) % 977 == 0`, e.g. i = 138: 7*138 + 11 = 977).
        // Identity = i64::MAX, so any worker chunk with zero matching
        // i values still combines correctly via the icmp+select fold.
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut m: i64 = 1000000i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let x: i64 = (i * 7i64 + 11i64) % 977i64;
        if x < m {
            m = x;
        }
        i = i + 1i64;
    }
    println(m);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "0");
    }

    #[test]
    fn test_e2e_reduction_max_conditional_assign_multi_worker() {
        // Mirror of the Min E2E for Max. Series `(i * 7 + 11) % 977`
        // for i in [0, 100_000) hits 976 at multiple i values; identity
        // = i64::MIN ensures empty-chunk workers don't poison the fold.
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut m: i64 = -1000000i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let x: i64 = (i * 7i64 + 11i64) % 977i64;
        if x > m {
            m = x;
        }
        i = i + 1i64;
    }
    println(m);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "976");
    }

    #[test]
    fn test_e2e_reduction_min_initial_value_preserved_when_no_smaller() {
        // Edge case: the initial accumulator value is smaller than any
        // loop value. Worker partials all start at identity = i64::MAX,
        // none gets updated (because every `x` is >= initial m). The
        // final combine fold reduces partials to identity, then the
        // par_reduce caller's final-combine step folds that with the
        // parent's initial m — producing m unchanged. Pins that the
        // initial-value-preservation path works end-to-end.
        let src = r#"
fn main() {
    let k_iters: i64 = 1000i64;
    let mut m: i64 = -999999i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let x: i64 = i + 1i64;
        if x < m {
            m = x;
        }
        i = i + 1i64;
    }
    println(m);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "-999999");
    }

    // ── Slice: conditional accumulator-update reduction (2026-05-20) ──
    //
    // The analyzer-side recognition tests in `tests/concurrency.rs` cover
    // shape acceptance. These e2e tests pin the codegen path: an outer
    // reduction shaped `if cond { acc = acc + delta; }` must fan out via
    // `karac_par_reduce`, each worker accumulating into a private slot,
    // final combine folding back into the parent's `sum`. The sink must
    // match the serial computation — since `+` is associative+commutative,
    // combine order doesn't matter.

    #[test]
    fn test_e2e_reduction_conditional_acc_update_assign_matches_serial() {
        // Count of i in [0, 99999] where i % 3 == 0:
        // 0, 3, 6, ..., 99999 → (99999/3)+1 = 33334.
        // The body has no Index/FieldAccess so the memory-bound gate
        // doesn't fire; K=100_000 × ~5 per-iter ops clears the cost gate,
        // so par_reduce dispatches.
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut sum: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let cond: bool = (i % 3i64) == 0i64;
        if cond {
            sum = sum + 1i64;
        }
        i = i + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "33334");
    }

    #[test]
    fn test_e2e_reduction_conditional_acc_update_compound_matches_serial() {
        // CompoundAssign variant: `sum += 1i64` inside the if-arm.
        // Same workload shape, same expected count (33334).
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut sum: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let cond: bool = (i % 3i64) == 0i64;
        if cond {
            sum += 1i64;
        }
        i = i + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "33334");
    }

    #[test]
    fn test_e2e_reduction_conditional_acc_update_with_variable_delta() {
        // The delta side can be a non-literal expression as long as it
        // doesn't reference the accumulator. Sum of `i*2` over i in [0,
        // 99999] where i is even:
        //   i = 0, 2, 4, ..., 99998 (50000 even values)
        //   delta_sum = sum(i * 2) for i in {0, 2, ..., 99998}
        //             = 2 * sum(0, 2, 4, ..., 99998)
        //             = 2 * 2 * sum(0, 1, 2, ..., 49999)
        //             = 4 * (49999 * 50000 / 2) = 4_999_900_000
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut sum: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let is_even: bool = (i % 2i64) == 0i64;
        if is_even {
            sum = sum + (i * 2i64);
        }
        i = i + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "4999900000");
    }

    #[test]
    fn test_e2e_reduction_two_arm_acc_update_matches_serial() {
        // 2026-05-20 follow-on: two-arm shape where both arms write the
        // same accumulator with the same op but different deltas.
        // Workload: for i in [0, 100_000), add 3 when i%3==0 else add 1.
        // Count of i where i%3==0 in [0, 100_000) is 33334 (0, 3, ..., 99999).
        //   3 * 33334 + 1 * (100000 - 33334) = 100002 + 66666 = 166668.
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut sum: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let hit: bool = (i % 3i64) == 0i64;
        if hit { sum = sum + 3i64; } else { sum = sum + 1i64; }
        i = i + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "166668");
    }

    #[test]
    fn test_e2e_reduction_two_arm_acc_update_compound_matches_serial() {
        // CompoundAssign in both arms. Same expected sink as above.
        let src = r#"
fn main() {
    let k_iters: i64 = 100000i64;
    let mut sum: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < k_iters {
        let hit: bool = (i % 3i64) == 0i64;
        if hit { sum += 3i64; } else { sum += 1i64; }
        i = i + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "166668");
    }

    // ── Bug B-2026-06-12-7: `for _` (wildcard) reduction lowering ────
    //
    // `extract_loop_shape` matched only `PatternKind::Binding` for the
    // for-form, so `for _ in 0..N { ...reduction... }` fell back to
    // sequential while the *identical* body under `for i in 0..N` (or the
    // while-form) parallelized. The loop variable is unused under a
    // wildcard, so the reduction is just as order-independent — the
    // discriminator was the loop-pattern, NOT the indexed body the
    // original triage suspected (the memory-bound gate never fires here;
    // the `work` call is substantial). Fix: synthesize a sentinel loop-var
    // name for the wildcard so the same fan-out path runs.

    #[test]
    fn test_ir_reduction_for_wildcard_emits_par_reduce() {
        // `for _ in 0..N` with a compute-heavy callee + indexed body must
        // emit the par_reduce fan-out, matching the `for i` / while forms.
        let ir = ir_for_with_concurrency(
            r#"
fn work(seed: i64) -> Array[i64, 2] {
    let mut a: i64 = 0i64;
    for j in 0i64..64i64 {
        a = a + (seed + j) * (seed - j);
    }
    Array[a, a + seed]
}
fn main() {
    let mut sum: i64 = 0i64;
    for _ in 0i64..5000i64 {
        let r = work(7i64);
        sum = sum + r[0] + r[1];
    }
    println(sum);
}
"#,
        );
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "for _ wildcard reduction should parallelize like for i / while. Got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_reduction_for_wildcard_indexed_matches_serial() {
        // The parallelized `for _` sink must equal the serial value.
        let src = r#"
fn work(seed: i64) -> Array[i64, 2] {
    let mut a: i64 = 0i64;
    for j in 0i64..64i64 {
        a = a + (seed + j) * (seed - j);
    }
    Array[a, a + seed]
}
fn main() {
    let mut sum: i64 = 0i64;
    for _ in 0i64..5000i64 {
        let r = work(7i64);
        sum = sum + r[0] + r[1];
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "-822045000");
    }

    // ── Slice: cost-gate function-call body cost (2026-05-20) ────────
    //
    // Codegen's cost-model gate used to treat every function/method call
    // as `CALL_COST_UNITS = 10` regardless of what the callee did,
    // bottoming out the K=N-small-outer + heavy-callee patterns
    // (kata-121's `for _ in 0..10 { sum = sum + max_profit(slice); }`).
    // The slice plumbs `Program.items`-derived free-fn bodies into a
    // `CostEstimator` struct that recursively estimates known callees
    // up to `INLINE_DEPTH_CAP = 3` levels, falling back to `CALL_COST_UNITS`
    // when callee shape is opaque (method, multi-segment Path, past cap).

    #[test]
    fn test_ir_cost_gate_trivial_inlined_callee_still_blocks() {
        // Counterpart to slice-3b.5's `test_ir_cost_gate_blocks_small_loop_with_method_call`:
        // callee body has cost ~1 (single binary op), per-iter post-inline
        // = 1 (body) + 1 (caller's `sum + ...`) = 2; K=100 → 200 unit-iters,
        // well below the 80,000 threshold. Pins that inlining a trivial
        // callee doesn't accidentally unlock parallelism for small loops.
        let src = r#"
fn double(x: i64) -> i64 { x + x }
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..100i64 {
        sum = sum + double(k);
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            !ir.contains("call void @karac_par_reduce"),
            "K=100 with trivial inlined callee still below gate; should block. Got:\n{ir}"
        );
    }

    #[test]
    fn test_ir_cost_gate_inlined_callee_with_inner_loop_unlocks_parallelism() {
        // Callee `helper()` has a literal-bounded inner for-loop. The
        // inner is recognized as a reduction but its own cost gate
        // (K=50, body cost ≈ 2, total = 100) blocks it, so the inner
        // never emits a par_reduce call regardless of inlining. That
        // isolation lets the OUTER `for k in 0..3000` be the discriminator
        // — par_reduce appears in the IR iff the outer's gate passes,
        // which only happens when the cost estimator inlines helper()'s
        // body cost (~33 units via NESTED_LOOP_MULTIPLIER on the inner
        // for) instead of the opaque CALL_COST_UNITS = 10. Math:
        //   - Without inlining: per-iter = 1+0+1+0+10 = 12; K=3000 → 36000 < 80000 → block
        //   - With inlining:    per-iter = 1+0+1+0+33 = 35; K=3000 → 105000 ≥ 80000 → pass
        let src = r#"
fn helper() -> i64 {
    let mut acc: i64 = 0i64;
    for j in 0i64..50i64 {
        acc = acc + j;
    }
    acc
}
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..3000i64 {
        sum = sum + helper();
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "K=3000 outer with inlined helper() body cost (~33) should pass cost gate; got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_cost_gate_inlined_callee_correctness() {
        // E2E counterpart of the IR test above — verifies the par_reduce
        // path produces the correct sink when the inlining-aware gate
        // unlocks parallelism.
        let src = r#"
fn helper() -> i64 {
    let mut acc: i64 = 0i64;
    for j in 0i64..50i64 {
        acc = acc + j;
    }
    acc
}
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..3000i64 {
        sum = sum + helper();
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ j for j in [0, 50) = 49 * 50 / 2 = 1225
        // K=3000 outer * 1225 per helper call = 3_675_000
        assert_eq!(out.trim(), "3675000");
    }

    // ── Runtime-loop trip-count calibration (RUNTIME_NESTED_LOOP_MULTIPLIER) ──
    // The flat `NESTED_LOOP_MULTIPLIER = 16` underestimated runtime-bounded
    // (non-const-range) loops by orders of magnitude, so a `for _ in 0..K`
    // reduction over a callee whose hot path is a *doubly-nested* runtime
    // scan (kata-28 `str_str`: `while i { while j { s[i+j] == n[j] } }`) scored
    // ≈30k cost units (16² × body × K=10) and fell under the 80k dispatch
    // threshold — declining a real ~11× parallel win to a serial run. The
    // calibration (`RUNTIME_NESTED_LOOP_MULTIPLIER = 64`, src/codegen/reduce.rs)
    // makes a doubly-nested runtime callee cross the gate at small K while a
    // single runtime loop at small K stays conservatively serial. Surfaced by
    // the 2026-06-13 `for _` auto-par re-bench sweep (phase-7-codegen.md).
    // Both callees use pure-compute while-loops (no `Index`) so the separate
    // memory-bound gate doesn't confound the cost-gate outcome under test.

    #[test]
    fn test_ir_cost_gate_doubly_nested_runtime_callee_fires_at_small_k() {
        // K=10 outer; callee has a doubly-nested runtime `while` (≈64² × body
        // per call ≫ 80k/K). Pins the calibration fix: this would NOT have
        // emitted par_reduce under the old flat-16 multiplier (16² × body × 10
        // ≈ 30k < 80k). Mirrors kata-28's str_str shape.
        let src = r#"
fn scan2(a: i64, b: i64) -> i64 {
    let mut i: i64 = 0i64;
    let mut acc: i64 = 0i64;
    while i < a {
        let mut j: i64 = 0i64;
        while j < b {
            acc = acc + i * j;
            j = j + 1i64;
        }
        i = i + 1i64;
    }
    acc
}
fn main() {
    let mut sum: i64 = 0i64;
    for _ in 0i64..10i64 {
        sum = sum + scan2(40i64, 40i64);
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "K=10 outer over a doubly-nested runtime-loop callee should cross the \
             cost gate under RUNTIME_NESTED_LOOP_MULTIPLIER; got:\n{ir}"
        );
    }

    #[test]
    fn test_ir_cost_gate_single_runtime_loop_callee_blocks_at_small_k() {
        // Over-fire guard / calibration ceiling: K=10 outer; callee has a
        // SINGLE runtime `while` (≈64 × body × K=10 ≈ 4.5k < 80k). Stays
        // serial — raising the multiplier must not make every runtime loop
        // fire at small K (the kata-1 hash_map case: lone `for i in 0..n`).
        let src = r#"
fn scan1(a: i64) -> i64 {
    let mut i: i64 = 0i64;
    let mut acc: i64 = 0i64;
    while i < a {
        acc = acc + i * 2i64;
        i = i + 1i64;
    }
    acc
}
fn main() {
    let mut sum: i64 = 0i64;
    for _ in 0i64..10i64 {
        sum = sum + scan1(40i64);
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            !ir.contains("call void @karac_par_reduce"),
            "K=10 over a single runtime-loop callee is below the cost gate; \
             should stay serial (no over-fire); got:\n{ir}"
        );
    }

    #[test]
    fn test_e2e_doubly_nested_runtime_callee_par_matches_serial() {
        // Sink-correctness for the calibration fire path above.
        let src = r#"
fn scan2(a: i64, b: i64) -> i64 {
    let mut i: i64 = 0i64;
    let mut acc: i64 = 0i64;
    while i < a {
        let mut j: i64 = 0i64;
        while j < b {
            acc = acc + i * j;
            j = j + 1i64;
        }
        i = i + 1i64;
    }
    acc
}
fn main() {
    let mut sum: i64 = 0i64;
    for _ in 0i64..10i64 {
        sum = sum + scan2(40i64, 40i64);
    }
    println(sum);
}
"#;
        // Σ_{i<40} Σ_{j<40} i*j = (Σi)² for i,j in [0,40) = (780)² = 608400
        // per call; × K=10 = 6_084_000.
        let Some(out) = run_program(src) else { return };
        assert_eq!(out.trim(), "6084000");
    }

    #[test]
    fn test_ir_memory_bound_body_skips_par_reduce() {
        // kata-153's find_min shape — body has an Index (nums[i]) and
        // only minimal compute (cond + assign), no function calls. The
        // memory-bound gate (slice: memory-bound rejection, 2026-05-20)
        // detects this and skips the lowering, falling back to
        // sequential codegen. Without this gate, the runtime cost gate
        // would dispatch (per_iter * iter_total = 10M > 180k threshold)
        // for no wall-clock benefit (the workload is bandwidth-bound),
        // paying ~3.4× User-CPU and +262 KiB binary.
        let src = r#"
fn find_min(nums: Slice[i64]) -> i64 {
    let n = nums.len();
    let mut m = nums[0];
    for i in 1i64..n {
        let x = nums[i];
        if x < m {
            m = x;
        }
    }
    m
}
fn main() {
    let mut v: Vec[i64] = Vec.filled(10i64, 5i64);
    println(find_min(v.as_slice()));
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            !ir.contains("call void @karac_par_reduce"),
            "memory-bound find_min body (Index + minimal compute, no call) should be \
             rejected by the memory-bound gate; got:\n{ir}"
        );
    }

    #[test]
    fn test_ir_memory_bound_gate_allows_substantial_call() {
        // Mirror of the memory-bound test: body has Index AND a free-fn
        // Call. The call signals "compute work beyond the memory access"
        // — the gate doesn't fire, the lowering proceeds (subject to
        // the cost-model gates). Test asserts par_reduce IS emitted to
        // pin that has-call escapes the memory-bound rejection.
        let src = r#"
fn heavy(x: i64) -> i64 {
    let mut acc: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < x {
        acc = acc + k;
        k = k + 1i64;
    }
    acc
}
fn main() {
    let v: Vec[i64] = Vec.filled(1000i64, 5i64);
    let s = v.as_slice();
    let mut sum: i64 = 0i64;
    for k in 0i64..3000i64 {
        sum = sum + s[k % 1000i64] + heavy(50i64);
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        // The body has both Index (s[k % 1000]) and a substantial Call
        // (heavy(50)) — the call signal trumps the index signal, so
        // the memory-bound gate doesn't fire and the cost-model lets
        // it through.
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "body with both Index and substantial Call should bypass memory-bound gate; got:\n{ir}"
        );
    }

    // ── Const-prop into par-reduce captured env ──────────────────────
    //
    // When a captured variable is initialized from a literal integer
    // (`let n: i64 = 8i64;`) and is never reassigned before the loop,
    // the worker fn should materialize it as an LLVM constant directly
    // — not load it from the par-reduce env-struct. This lets LLVM see
    // through subsequent uses (e.g. `k % n` where n is a power-of-two
    // folds to an `and`-mask instead of a runtime `sdiv`/`msub`). The
    // gap surfaced on the kata-8 atoi bench where the natural source
    // `let n: i64 = 8i64; ... while k < K { idx = k % n; ... }` was
    // ~9% slower than rewriting it as `idx = k % 8i64` purely because
    // the descriptor boundary opaqued the constant. Tests pin:
    //   1. Const-init capture: worker IR contains a `store i64 <lit>`
    //      of the literal value (the per-worker alloca init) — i.e. the
    //      constant *did* flow into the worker body.
    //   2. Mut capture: worker IR still reads it from the env-struct
    //      `extractvalue` chain (no const-prop unsoundly applied).
    //   3. Mixed: both paths coexist in one reduction site.
    //   4. E2E: const-prop doesn't change the sink for any of the above.
    /// Slice the IR text down to the body of the first synthesized
    /// `__karac_reduce_worker_*` fn so capture-related assertions don't
    /// match the outer fn's own copies of the captured stores (main's
    /// own `let n = 8` lowers to a `store i64 8, ptr %n` too — that
    /// store is *not* the const-prop we're testing for). Returns the
    /// substring from the worker fn's `define void @...worker_N(...) {`
    /// header up through its closing `}`.
    fn extract_first_reduce_worker_body(ir: &str) -> String {
        // The worker fn's definition line — distinct from any *call* /
        // *reference* to `@__karac_reduce_worker_` from main's
        // descriptor setup.
        let header_needle = "define void @__karac_reduce_worker_";
        let header_start = ir
            .find(header_needle)
            .unwrap_or_else(|| panic!("no `{header_needle}` in IR:\n{ir}"));
        let after_header = &ir[header_start..];
        // The body is everything from the `{` opening this fn up to the
        // matching `}`. LLVM IR fn bodies don't nest braces past the
        // top level, so a balanced-brace walk just bumps a depth counter.
        let body_open = after_header.find('{').expect("worker fn header has no `{`");
        let mut depth: i32 = 0;
        let mut end_off: Option<usize> = None;
        for (i, ch) in after_header[body_open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_off = Some(body_open + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end_off.expect("unbalanced braces in worker fn body");
        after_header[..end].to_string()
    }

    #[test]
    fn test_ir_reduction_const_int_capture_inlined_in_worker() {
        // `let n = 8i64;` non-mut + literal init → const-prop kicks in.
        // The worker fn should store `i64 8` into the worker-local `n`
        // alloca rather than extract it from the env-struct.
        let src = r#"
fn main() {
    let n: i64 = 8i64;
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + (k % n);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");

        let worker = extract_first_reduce_worker_body(&ir);
        // The const value 8 must be stored into the worker-local `n`
        // alloca. The exact alloca name follows the source-level
        // variable name, so look for `store i64 8, ptr %n` *inside the
        // worker fn body* (not the outer fn's own let-init store).
        assert!(
            worker.contains("store i64 8, ptr %n"),
            "expected const-init capture `n = 8` to be materialized as \
             `store i64 8, ptr %n` in the worker fn body; worker:\n{worker}"
        );
        // With `n` const-propped out, the worker should not load from
        // an env-struct (no extractvalue over a `__reduce_env_load`).
        assert!(
            !worker.contains("__reduce_env_load"),
            "with the sole capture const-propped, no env-struct load \
             should appear in the worker; worker:\n{worker}"
        );
    }

    #[test]
    fn test_ir_reduction_per_iter_drops_emitted_in_worker_body() {
        // Regression guard for the par_reduce per-iteration heap leak.
        // `let result = <call returning Vec[T]>` inside the reduction
        // body must drop `result` at the end of each iteration — without
        // a per-iteration cleanup frame in `emit_reduce_worker_fn`, the
        // drop registers against the worker's top frame which only drains
        // at `exit_bb`, so every iteration's heap allocation leaks until
        // the last one (whose `result` ptr is what the single exit-time
        // drop runs on). Peak RSS on kata 6 (zigzag conversion) hit
        // 498 MiB pre-fix vs 1.5 MiB on the seq lane; post-fix 6 MiB
        // (worker stacks + per-worker partial accumulators).
        //
        // Structural check: the worker's `loop.body` block must contain
        // at least one `call .* @free` (drop call) before its back-edge
        // `br label %loop.cond`. The leaky shape had `@free` only inside
        // `loop.exit` (after the back-edge), so this fact pins the fix.
        let src = r#"
fn make_vec(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.with_capacity(n);
    let mut i = 0i64;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    v
}
fn main() {
    let mut sum = 0i64;
    let mut k = 0i64;
    while k < 1000i64 {
        let result = make_vec(64);
        sum = sum + result.len();
        k = k + 1;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");

        let worker = extract_first_reduce_worker_body(&ir);
        // The worker must contain a `loop.body` block AND a back-edge
        // `br label %loop.cond`. Without the auto-par dispatch firing
        // there'd be no worker fn at all — `extract_first_reduce_worker_body`
        // would have panicked above.
        assert!(
            worker.contains("loop.body:"),
            "worker fn missing loop.body block; worker:\n{worker}"
        );
        assert!(
            worker.contains("br label %loop.cond"),
            "worker fn missing back-edge to loop.cond; worker:\n{worker}"
        );

        // Find the byte offset of the back-edge to loop.cond and the
        // offset of `loop.body:`. The leak-fix's per-iteration drop must
        // appear in the IR text BETWEEN those two — that's the slice
        // emitted inside `loop.body` after `compile_block(body)` and
        // before the back-edge `br`. The leak shape would have any
        // `@free` calls only in `loop.exit:` (after the back-edge).
        let body_start = worker
            .find("loop.body:")
            .expect("loop.body label not found above");
        let backedge_pos = worker[body_start..]
            .find("br label %loop.cond")
            .map(|p| body_start + p)
            .expect("loop.cond back-edge not found inside loop.body");
        let body_slice = &worker[body_start..backedge_pos];
        assert!(
            body_slice.contains("call void @free("),
            "expected at least one `call void @free(...)` between `loop.body:` \
             and the back-edge `br label %loop.cond` (per-iter cleanup drain) — \
             this is the leak-fix invariant. body_slice:\n{body_slice}"
        );
    }

    #[test]
    fn test_ir_reduction_mut_capture_stays_runtime() {
        // `let mut n = 8i64;` mut → const-prop must not apply (could
        // be reassigned). Worker should still read `n` from the env
        // struct, no `store i64 8, ptr %n` literal in the worker body.
        let src = r#"
fn main() {
    let mut n: i64 = 8i64;
    n = 9i64;
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + (k % n);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");

        let worker = extract_first_reduce_worker_body(&ir);
        // No const-prop store of either initializer value inside the
        // worker fn (the outer fn's let/assign stores must not bleed
        // into this assertion — hence the extracted worker slice).
        assert!(
            !worker.contains("store i64 8, ptr %") && !worker.contains("store i64 9, ptr %"),
            "mut capture must NOT be const-propped into the worker; worker:\n{worker}"
        );
        // The env-struct load path must be live for the runtime capture.
        assert!(
            worker.contains("__reduce_env_load"),
            "expected env-struct load for the runtime capture in worker; worker:\n{worker}"
        );
    }

    #[test]
    fn test_ir_reduction_mixed_const_and_runtime_captures() {
        // `n` (const-init) + `factor` (mut, runtime) — the worker should
        // store i64 8 for n AND load factor from the env-struct.
        let src = r#"
fn main() {
    let n: i64 = 8i64;
    let mut factor: i64 = 3i64;
    factor = 5i64;
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + (k % n) * factor;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");

        let worker = extract_first_reduce_worker_body(&ir);
        assert!(
            worker.contains("store i64 8, ptr %n"),
            "expected const-init `n=8` to land as a constant store in the \
             worker; worker:\n{worker}"
        );
        assert!(
            worker.contains("__reduce_env_load"),
            "expected env-struct load to be live for runtime `factor` \
             capture in worker; worker:\n{worker}"
        );
    }

    // ── Loop-var non-negative hint for SCEV ──────────────────────────
    //
    // When the par-reduce worker's loop var starts at the chunk start
    // (`lo_in_worker.is_none()`), the runtime passes start as usize
    // — provably non-negative. Codegen emits a `llvm.assume(start >= 0)`
    // at fn entry so SCEV can prove `k >= 0` throughout the loop. With
    // that fact, InstCombine folds signed-mod / signed-div by positive
    // power-of-two literals (`srem k, N` → `urem k, N` → `and k, N-1`).
    // Surfaced on kata-8 atoi's `idx = k % 8` inner expression which
    // was emitting the 4-instruction signed-mod chain on ARM64
    // (negs/and/and/csneg) instead of a single `and`.
    #[test]
    fn test_ir_reduction_worker_assumes_loop_var_nonneg() {
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + k;
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        let worker = extract_first_reduce_worker_body(&ir);
        assert!(
            worker.contains("call void @llvm.assume"),
            "expected llvm.assume at worker entry; worker:\n{worker}"
        );
        assert!(
            worker.contains("k.start.nonneg"),
            "expected the non-negative compare named `k.start.nonneg` \
             (whose result feeds llvm.assume); worker:\n{worker}"
        );
    }

    #[test]
    fn test_e2e_reduction_worker_nonneg_sink_matches() {
        // The assume must not change program semantics — sink stays
        // the same Σ formula for k in [0, K).
        let src = r#"
fn main() {
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + (k % 8i64);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ (k % 8) for k in [0, 100000): 12500 cycles × 28 = 350000.
        assert_eq!(out.trim(), "350000");
    }

    // ── Bounds-check elision via hoisted modulo preflight ──────────
    //
    // For `let idx = k % LIT` (where k is the par-reduce loop var, hence
    // non-negative per the assume above) and `captured_vec[idx]` in the
    // worker body, codegen emits a one-time `if vec.len() < LIT panic`
    // preflight at fn entry. The preflight proves `LIT <= vec.len()`,
    // which combined with `idx ∈ [0, LIT)` proves `idx < vec.len()`
    // unconditionally. The per-iter bounds check on `vec[idx]` is then
    // dropped via the existing `asserted_index_bounds` mechanism.
    //
    // Recognizes both an integer-literal divisor (`k % 8i64`) and a
    // const-int-capture divisor (`k % n` where `let n = 8i64;` was
    // const-propped). Conservative: skips when the vec is mutated in
    // the body, when the let is mut, or when the index is computed
    // in a non-top-level position.
    #[test]
    fn test_ir_reduction_modulo_bce_literal_divisor() {
        // `compute` is a substantial fn call so the body bypasses the
        // memory-bound gate (which would skip the par_reduce lowering
        // for an Index-only body with no compute).
        let src = r#"
fn compute(x: i64) -> i64 {
    x * x + 1i64
}

fn main() {
    let mut inputs: Vec[i64] = Vec.new();
    inputs.push(10i64);
    inputs.push(20i64);
    inputs.push(30i64);
    inputs.push(40i64);
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        let idx: i64 = k % 4i64;
        sum = sum + compute(inputs[idx]);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        let worker = extract_first_reduce_worker_body(&ir);
        // Preflight block + length compare against the literal 4.
        assert!(
            worker.contains("bce.preflight.fail"),
            "expected hoisted BCE preflight block; worker:\n{worker}"
        );
        assert!(
            worker.contains("bce.preflight.cmp"),
            "expected the preflight len-compare; worker:\n{worker}"
        );
        // No per-iter bounds-check labels — both halves elided.
        assert!(
            !worker.contains("vidx.oob"),
            "expected the per-iter bounds-check to be elided; worker:\n{worker}"
        );
    }

    #[test]
    fn test_ir_reduction_modulo_bce_const_capture_divisor() {
        // Bench-shape pattern: `let n: i64 = 8i64;` is const-propped
        // through the par-reduce capture (see test_ir_reduction_
        // const_int_capture_inlined_in_worker), then `idx = k % n`
        // resolves `n` through the const-int-capture lookup. BCE
        // recovers the same upper bound. `compute` bypasses the
        // memory-bound gate the same way as the literal-divisor test.
        let src = r#"
fn compute(x: i64) -> i64 {
    x * x + 1i64
}

fn main() {
    let n: i64 = 4i64;
    let mut inputs: Vec[i64] = Vec.new();
    inputs.push(10i64);
    inputs.push(20i64);
    inputs.push(30i64);
    inputs.push(40i64);
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        let idx: i64 = k % n;
        sum = sum + compute(inputs[idx]);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        let worker = extract_first_reduce_worker_body(&ir);
        assert!(
            worker.contains("bce.preflight.fail"),
            "const-capture divisor: expected hoisted BCE preflight; worker:\n{worker}"
        );
        assert!(
            !worker.contains("vidx.oob"),
            "const-capture divisor: per-iter bounds-check should be elided; worker:\n{worker}"
        );
    }

    #[test]
    fn test_ir_reduction_modulo_bce_skipped_when_vec_mutated() {
        // If the body mutates the captured vec (push, index-assign, etc.),
        // BCE is unsound — len could change between iters. Conservative
        // rule: skip BCE entirely when any method call appears on the
        // captured vec name. Here `inputs.push(k)` triggers the skip.
        let src = r#"
fn main() {
    let mut inputs: Vec[i64] = Vec.new();
    inputs.push(10i64);
    inputs.push(20i64);
    inputs.push(30i64);
    inputs.push(40i64);
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        let idx: i64 = k % 4i64;
        sum = sum + inputs[idx];
        inputs.push(k);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        // Note: this source may fail the recognizer's effect/ownership
        // checks (mutating a captured vec from a reduction body could be
        // disallowed). If so, the test still has value as a "doesn't
        // crash" guard — the compile_to_ir call below uses .ok() to
        // swallow either a clean compile (assertion-checked below) or
        // an upstream error (we simply skip the assertions).
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let Ok(ir) = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        ) else {
            // Codegen rejected upstream — fine; the mutate-while-reducing
            // shape may be guarded out elsewhere. The point of this test
            // is "BCE doesn't unsoundly fire", so absence of a worker fn
            // is also a pass.
            return;
        };
        // If a worker was synthesized, it must NOT have a preflight (since
        // the vec is mutated; BCE rules say skip).
        if ir.contains("@__karac_reduce_worker_") {
            let worker = extract_first_reduce_worker_body(&ir);
            assert!(
                !worker.contains("bce.preflight"),
                "mutated vec: BCE preflight must NOT be emitted; worker:\n{worker}"
            );
        }
    }

    #[test]
    fn test_e2e_reduction_modulo_bce_sink_matches() {
        // BCE must not change semantics. The sink is the same Σ formula
        // regardless of whether the per-iter check ran or got elided.
        // `compute` is the same memory-bound-gate bypass as the IR
        // tests above.
        let src = r#"
fn compute(x: i64) -> i64 {
    x * x + 1i64
}

fn main() {
    let mut inputs: Vec[i64] = Vec.new();
    inputs.push(10i64);
    inputs.push(20i64);
    inputs.push(30i64);
    inputs.push(40i64);
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        let idx: i64 = k % 4i64;
        sum = sum + compute(inputs[idx]);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ compute(inputs[k%4]) for k in [0, 100000):
        // 25000 cycles × (101 + 401 + 901 + 1601) = 25000 × 3004 = 75_100_000
        assert_eq!(out.trim(), "75100000");
    }

    #[test]
    fn test_e2e_reduction_const_capture_sink_matches() {
        // Const-prop must not change the program's output.
        let src = r#"
fn main() {
    let n: i64 = 8i64;
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 100000i64 {
        sum = sum + (k % n);
        k = k + 1i64;
    }
    println(sum);
}
"#;
        let Some(out) = run_program(src) else { return };
        // Σ (k % 8) for k in [0, 100000):
        // 100000 / 8 = 12500 complete cycles, each summing 0+1+2+...+7 = 28.
        // 12500 * 28 = 350000. No partial cycle since 100000 is a multiple of 8.
        assert_eq!(out.trim(), "350000");
    }

    #[test]
    fn test_ir_cost_gate_recursive_callee_terminates_via_depth_cap() {
        // Indirect recursion `a() -> b()`, `b() -> a()` — the inliner
        // must terminate (not infinite-loop) and fall back to the
        // CALL_COST_UNITS estimate past the depth cap. Test passes if
        // compilation completes (codegen doesn't hang) regardless of
        // whether the gate ends up blocking or passing.
        let src = r#"
fn a(n: i64) -> i64 { if n > 0 { b(n - 1i64) } else { 0i64 } }
fn b(n: i64) -> i64 { if n > 0 { a(n - 1i64) } else { 0i64 } }
fn main() {
    let mut sum: i64 = 0i64;
    for k in 0i64..10i64 {
        sum = sum + a(k);
    }
    println(sum);
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        // No assertion on the gate outcome — just that codegen
        // terminates within a reasonable bound (the test harness's
        // implicit timeout). If the depth cap is broken, this hangs.
        let _ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed (recursion should bottom out at depth cap)");
    }

    // ── Phase 3 (2026-05-21): `#[par_unordered]` collect-style codegen ─────
    //
    // Tests for the Vec-accumulator code path in `src/codegen/reduce.rs`:
    // `try_emit_collect_reduction_lowering` + per-elem-type init/combine
    // helpers + Collect worker fn that owns a local Vec partial. Each test
    // pairs a sink-correctness check (sum / count are order-independent so
    // they survive the worker-combine reordering) with an IR pin that
    // proves the new helpers were synthesized rather than the sequential
    // fallback firing silently.

    #[test]
    fn test_e2e_collect_bare_push_matches_serial_sink() {
        // K=1000 pushes of `k` produce a Vec whose length is 1000 and whose
        // element sum is 0+1+...+999 = 499500. Order across workers is
        // unconstrained but length and sum are not.
        let src = r#"
fn main() {
    let mut results: Vec[i64] = Vec.new();
    #[par_unordered]
    for k in 0i64..1000i64 {
        results.push(k);
    }
    let mut s: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < results.len() {
        s = s + results[i];
        i = i + 1i64;
    }
    println(results.len());
    println(s);
}
"#;
        let Some(out) = run_program(src) else { return };
        let lines: Vec<&str> = out.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "expected two lines, got:\n{}", out);
        assert_eq!(lines[0], "1000", "length mismatch");
        assert_eq!(
            lines[1], "499500",
            "sum mismatch (length right but contents wrong?)"
        );
    }

    #[test]
    fn test_e2e_collect_conditional_push_matches_serial_sink() {
        // Conditional push: only k where k % 3 == 0 is pushed. For k in
        // [0, 1000): count = ceil(1000/3) = 334 (0, 3, 6, ..., 999).
        // Sum = 3 * (0 + 1 + ... + 333) = 3 * 333 * 334 / 2 = 166833.
        let src = r#"
fn main() {
    let mut hits: Vec[i64] = Vec.new();
    #[par_unordered]
    for k in 0i64..1000i64 {
        if (k % 3i64) == 0i64 {
            hits.push(k);
        }
    }
    let mut s: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < hits.len() {
        s = s + hits[i];
        i = i + 1i64;
    }
    println(hits.len());
    println(s);
}
"#;
        let Some(out) = run_program(src) else { return };
        let lines: Vec<&str> = out.trim().split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "334", "conditional count mismatch");
        assert_eq!(lines[1], "166833", "conditional sum mismatch");
    }

    #[test]
    fn test_e2e_collect_preserves_parent_initial_items() {
        // Parent acc starts with two pre-existing items pushed before the
        // par-unordered loop. Post-call fold extends out_slot's Vec into
        // the parent acc, so the final length is 2 (pre) + 100 (loop) =
        // 102, and the sum is (-1) + (-2) + sum(0..99) = -3 + 4950 = 4947.
        // The pre-existing items appear first in the dst per combine_fn's
        // "dst before src" memcpy order.
        let src = r#"
fn main() {
    let mut acc: Vec[i64] = Vec.new();
    acc.push(0i64 - 1i64);
    acc.push(0i64 - 2i64);
    #[par_unordered]
    for k in 0i64..100i64 {
        acc.push(k);
    }
    let mut s: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < acc.len() {
        s = s + acc[i];
        i = i + 1i64;
    }
    println(acc.len());
    println(s);
}
"#;
        let Some(out) = run_program(src) else { return };
        let lines: Vec<&str> = out.trim().split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0], "102",
            "parent-initial items dropped from final length"
        );
        assert_eq!(
            lines[1], "4947",
            "parent-initial values not folded into sum"
        );
    }

    #[test]
    fn test_ir_collect_emits_par_reduce_call_and_collect_helpers() {
        // IR pin: the `#[par_unordered]` collect lowering must emit a
        // call to karac_par_reduce plus the per-elem-type init/combine
        // helpers (`__karac_reduce_init_collect_i64`,
        // `__karac_reduce_combine_collect_i64`) and a Collect-specific
        // worker symbol (`__karac_reduce_worker_collect_<N>`). Without
        // this pin, a silent fallthrough to sequential codegen would
        // make the sink tests above still pass (sequential push gives
        // the same multiset) but the lowering would have regressed.
        let src = r#"
fn main() {
    let mut results: Vec[i64] = Vec.new();
    #[par_unordered]
    for k in 0i64..1000i64 {
        results.push(k);
    }
    println(results.len());
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            ir.contains("call void @karac_par_reduce"),
            "expected karac_par_reduce call for #[par_unordered] collect loop; got:\n{ir}"
        );
        assert!(
            ir.contains("__karac_reduce_init_collect_i64"),
            "expected init_collect_i64 helper; got:\n{ir}"
        );
        assert!(
            ir.contains("__karac_reduce_combine_collect_i64"),
            "expected combine_collect_i64 helper; got:\n{ir}"
        );
        assert!(
            ir.contains("__karac_reduce_worker_collect_"),
            "expected worker_collect_<N> fn; got:\n{ir}"
        );
    }

    #[test]
    fn test_ir_collect_without_attribute_stays_sequential() {
        // Regression guard: without `#[par_unordered]` the analyzer
        // doesn't tag `acc.push(k)` as a Collect reduction (the gate is
        // attribute-driven per Phase 2 design — order-not-preserved
        // requires explicit user opt-in). Codegen must fall through to
        // the sequential push path; no par_reduce call, no Collect
        // helpers.
        let src = r#"
fn main() {
    let mut results: Vec[i64] = Vec.new();
    for k in 0i64..1000i64 {
        results.push(k);
    }
    println(results.len());
}
"#;
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);
        let ir = karac::codegen::compile_to_ir_with_options(
            &parsed.program,
            None,
            Some(&analysis),
            None,
            None,
        )
        .expect("codegen failed");
        assert!(
            !ir.contains("__karac_reduce_init_collect_"),
            "no `#[par_unordered]` ⇒ no init_collect helper; got:\n{ir}"
        );
        assert!(
            !ir.contains("__karac_reduce_worker_collect_"),
            "no `#[par_unordered]` ⇒ no worker_collect fn; got:\n{ir}"
        );
    }

    // --- Par codegen slice 4: defer / errdefer on the cancel path ---
    //
    // Slice 4 of the Phase 7 § *Par codegen: cancellation and error
    // propagation* tranche. The single-line emission change in
    // `emit_branch_cancel_check` (`src/codegen/par_blocks.rs`) routes
    // the mid-branch cancel-check exit through
    // `emit_scope_cleanup_for_error_path` instead of
    // `emit_scope_cleanup`, so user `errdefer { ... }` blocks fire on
    // cooperative cancellation (matches design.md "observing
    // cancellation — `errdefer(e)` sees `e = Cancelled`"). User
    // `defer { ... }` blocks were already firing via the prior
    // `emit_scope_cleanup` drain; the switch keeps that behaviour
    // (defers drain in phase 2 of the error-path walk) while adding
    // errdefer-on-cancel.
    //
    // The defer-codegen entry's slice 2 split (which introduced
    // `UserErrDefer` skipping in `emit_scope_cleanup`) made the
    // slice-4 description's original "no new emission code" claim
    // stale — without the cancel-exit's switch to the error-path
    // drain, errdefers would have been silently swallowed on
    // cooperative cancel. These two tests pin both halves of the
    // resulting behaviour: the errdefer test is the discriminating
    // signal (errdefer ONLY fires on error paths, so observing the
    // body's stdout proves the cancel-exit is treated as an error
    // path); the defer test is the regression lock against any
    // future refactor that breaks defer-on-cancel via the new
    // error-path drain.

    #[test]
    fn test_e2e_par_branch_defer_fires_on_cooperative_cancel() {
        // Branch 0 runs a long loop of effectful `println(i)` calls;
        // each call's pre-call cancel-check (per slice 3's audit)
        // sees the cancel flag and routes to the cancel-exit BB.
        // Branch 1's `let b: Result[i64, i64] = fast_err();` is the
        // canonical slice-1a Result-typed-binding shape that
        // `branch_result_binding_name` recognises (annotation-only
        // detection: `PatternKind::Binding(name)` + `Result[...]`
        // type annotation) — slot-write at branch end stores the Err
        // tag and fires the cancel-flag store, which slice 1a's
        // codegen wires into the branch fn's body.
        //
        // With the slice-4 switch, branch 0's next mid-branch
        // cancel-check (before any `println(i)` after the cancel flag
        // is set) routes to the cancel-exit BB, which now drains the
        // registered defer body.
        //
        // The loop bound is sized for DETERMINISM under parallel test load,
        // not merely "big enough". The pool always has >= 2 workers
        // (`resolve_pool_workers().max(2)`), so branch 1 (`fast_err`, one
        // trivial `Err`) runs concurrently with branch 0 and sets the cancel
        // flag after ~one scheduler quantum, while branch 0 needs ~one quantum
        // PER ITERATION. A short loop (the old 10_000) let branch 0 sometimes
        // finish all its iterations before branch 1's runnable thread got a
        // CPU slice under heavy oversubscription — the pre-existing flake
        // (B-2026-07-08-6 investigation). A 1M-iteration ceiling makes that
        // impossible: fair scheduling cannot starve branch 1's runnable thread
        // for the multi-second span branch 0 would need to run 1M println's, so
        // branch 1 always sets the flag first and branch 0 cancels early
        // (typically after a few hundred iterations). The ceiling is only ever
        // REACHED if cancel is broken — the regression this test guards — so
        // the normal (passing) run stays fast; only a genuine failure pays the
        // full-loop cost.
        let out = run_program(
            r#"
fn fast_err() -> Result[i64, i64] { Err(99_i64) }

fn main() {
    par {
        let _ = {
            defer { println("def-fired-on-cancel"); }
            let mut i = 0_i64;
            while i < 1000000_i64 {
                println(i);
                i = i + 1_i64;
            }
            0_i64
        };
        let b: Result[i64, i64] = fast_err();
    }
}
"#,
        );
        if let Some(out) = out {
            assert!(
                out.contains("def-fired-on-cancel"),
                "defer body should fire when cancel-exit drains the branch frame; \
                 got first 500 chars:\n{}",
                &out[..out.len().min(500)],
            );
            // Sanity-check: branch 0 did NOT run to completion. If it had, the
            // cancel never fired and the defer drained via the normal-exit path
            // (which would also have printed the final `999999` before the
            // defer's `def-fired-on-cancel` line). `999999` cannot appear as a
            // substring of any smaller printed value (0..999999), so its absence
            // uniquely proves the cancel-exit was the drain site.
            assert!(
                !out.contains("999999"),
                "branch should have been cancelled before completing all iterations; \
                 saw `999999` in output (last 200 chars):\n{}",
                &out[out.len().saturating_sub(200)..],
            );
        }
    }

    #[test]
    fn test_e2e_par_branch_errdefer_fires_on_cooperative_cancel() {
        // Discriminating test for the slice-4 switch. Errdefer ONLY
        // fires on error paths (`emit_scope_cleanup` skips
        // `UserErrDefer` slots; only
        // `emit_scope_cleanup_for_error_path` runs them). The
        // branch's source-level value is `0_i64` (a normal Ok-ish
        // exit), so the only way the errdefer body runs is via the
        // cancel-exit drain. Observing `"err-fired-on-cancel"` in
        // output is the unique signal that the cancel-exit is now
        // recognised as an error path — pre-slice-4 (when
        // `emit_branch_cancel_check` called `emit_scope_cleanup`),
        // the errdefer would have been silently swallowed.
        // 1M-iteration ceiling for determinism under parallel test load — see
        // the sibling `..._defer_fires_on_cooperative_cancel` for the full
        // rationale (>= 2 workers → branch 1 sets the cancel flag after ~one
        // quantum; the large loop guarantees branch 0 can't finish first, so it
        // always exits via cancel). Here the errdefer is the discriminator on
        // its own: the branch value is a normal `0_i64` exit, so `errdefer`
        // (error-path only) fires IFF the cancel-exit path ran — no sentinel
        // check needed. A short loop let branch 0 sometimes complete NORMALLY,
        // where the errdefer correctly does NOT fire, flaking this assertion.
        let out = run_program(
            r#"
fn fast_err() -> Result[i64, i64] { Err(99_i64) }

fn main() {
    par {
        let _ = {
            errdefer { println("err-fired-on-cancel"); }
            let mut i = 0_i64;
            while i < 1000000_i64 {
                println(i);
                i = i + 1_i64;
            }
            0_i64
        };
        let b: Result[i64, i64] = fast_err();
    }
}
"#,
        );
        if let Some(out) = out {
            assert!(
                out.contains("err-fired-on-cancel"),
                "errdefer body should fire on cancel-exit (slice-4 switch to \
                 emit_scope_cleanup_for_error_path); got first 500 chars:\n{}",
                &out[..out.len().min(500)],
            );
        }
    }

    // ── L227: non-trivial captures (rc_inc for shared, ref/move handling) ──

    /// L227 IR: a par-block capturing a `shared struct` emits an
    /// atomic rc_inc in the branch prologue. This is the load-bearing
    /// signal that the ownership-pass classification fed through to
    /// codegen — without it, the branch's heap-pointer copy aliases
    /// the parent's reference with no inc, and a consume in the
    /// branch would race with the parent's scope-exit dec. The
    /// `atomicrmw add` op appearing inside `__par_branch_*` is the
    /// minimum proof that the SharedRc lowering path fired.
    #[test]
    fn test_ir_l227_shared_capture_emits_atomic_rc_inc_in_branch() {
        let ir = ir_for_with_pipeline(
            r#"
shared struct Counter { val: i64 }

fn main() {
    let c = Counter { val: 42 };
    par {
        println(c.val);
        println(99);
    }
}
"#,
        );
        // Locate the branch fn that references `c`. The branch
        // capturing `c` is the one whose body reads `c.val`; the
        // sibling branch (println(99)) takes no captures and won't
        // contain an atomic op. Slice the IR around the first
        // `__par_branch_` opening brace to bound the search.
        let branch_start = ir
            .find("define void @__par_branch_")
            .expect("expected par branch fn in IR");
        let branch_window = &ir[branch_start..];
        let branch_end = branch_window
            .find("\ndefine ")
            .map(|i| branch_start + i)
            .unwrap_or(ir.len());
        let branch_ir = &ir[branch_start..branch_end];
        assert!(
            branch_ir.contains("atomicrmw add"),
            "L227 SharedRc lowering must emit `atomicrmw add` in the branch \
             prologue for the captured shared struct; got branch IR:\n{}",
            &branch_ir[..branch_ir.len().min(2000)],
        );
    }

    /// L227 IR negative: a par-block capturing only a primitive
    /// (i64) keeps the existing Copy-by-value-through-env path and
    /// does NOT emit a rc_inc in the branch prologue. Guards against
    /// over-eager classification — the SharedRc path should fire
    /// only when the binding's surface type is a shared struct/enum.
    #[test]
    fn test_ir_l227_primitive_capture_no_atomic_rc_inc() {
        let ir = ir_for_with_pipeline(
            r#"
fn main() {
    let n: i64 = 42_i64;
    par {
        println(n);
        println(99);
    }
}
"#,
        );
        let branch_start = ir
            .find("define void @__par_branch_")
            .expect("expected par branch fn in IR");
        let branch_window = &ir[branch_start..];
        let branch_end = branch_window
            .find("\ndefine ")
            .map(|i| branch_start + i)
            .unwrap_or(ir.len());
        let branch_ir = &ir[branch_start..branch_end];
        assert!(
            !branch_ir.contains("atomicrmw"),
            "primitive captures should not trigger atomic rc ops in the par \
             branch (Copy-path only); got branch IR:\n{}",
            &branch_ir[..branch_ir.len().min(2000)],
        );
    }

    /// L227 E2E: a shared struct captured into a single par branch
    /// runs to completion without crashing or printing garbage, even
    /// when the branch reads the heap value while the parent's owning
    /// reference stays live. Pre-L227 this case "worked" by accident
    /// (the branch never bumped the refcount, so the parent's
    /// scope-exit dec hit the right count); with L227 the
    /// branch holds its own atomic +1 and dec's it on exit, so the
    /// refcount lifecycle is now correct-by-construction instead of
    /// correct-by-luck. The smoke test pinpoints the round trip:
    /// parent reads the field after par_run returns, which would
    /// touch freed memory if the refcount fell out of balance.
    #[test]
    fn test_e2e_l227_shared_capture_single_branch_lifecycle_ok() {
        let out = run_program(
            r#"
shared struct Counter { val: i64 }

fn main() {
    let c = Counter { val: 42 };
    par {
        println(c.val);
        println(99);
    }
    println(c.val);
}
"#,
        );
        if let Some(out) = out {
            assert!(
                out.contains("42"),
                "expected branch to print captured shared-struct field 42; got: {out:?}"
            );
            assert!(
                out.contains("99"),
                "expected sibling branch to print 99; got: {out:?}"
            );
            // The trailing parent read must succeed — pre-L227 this
            // could read freed memory if the branch's dec dropped
            // the count below the parent's owning reference. With
            // L227's inc/dec pairing, the parent's reference is
            // preserved across the par-run.
            assert_eq!(
                out.matches("42").count(),
                2,
                "expected `42` printed twice (branch + post-par parent read); got: {out:?}"
            );
        }
    }
    // ── Slot-ownership transfer (auto-par, 2026-06-05) ─────────────

    /// A branch that PUBLISHES an ownership-bearing value through a
    /// return slot must not also free it at branch end. Pre-fix, the
    /// `let name = "ka" + "ra"; let mut m: Map[String, i64] = Map.new();
    /// m.insert("a", 1);` shape auto-par'd `String.add` + `Map.new()`
    /// into one group, and the Map-producing branch ran its queued
    /// `FreeMapHandle` right after writing the handle into the return
    /// struct — the parent's `m.insert` was a use-after-free (SIGSEGV
    /// on a native build from main; surfaced by the phase-10 WASM
    /// build-path slice's cross-target probes). The fix removes the
    /// branch-side action and re-registers it against the parent's
    /// alloca (`SlotOwnership` transfer): the branch fn must contain
    /// ZERO map frees, the parent exactly one.
    #[test]
    fn test_auto_par_map_slot_branch_does_not_free_published_handle() {
        let ir = ir_for_with_concurrency(
            r#"
fn main() {
    let name = "ka" + "ra";
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
}
"#,
        );
        // The group must actually have fired — otherwise this test
        // silently validates sequential lowering.
        assert!(
            ir.contains("call void @karac_par_run"),
            "expected the String.add + Map.new group to auto-parallelize; IR:\n{ir}"
        );
        let fn_body = |name: &str| -> &str {
            let start = ir
                .find(&format!("define void @{name}("))
                .or_else(|| ir.find(&format!("define i32 @{name}(")))
                .unwrap_or_else(|| panic!("{name} not found in IR:\n{ir}"));
            let end = ir[start..].find("\n}").map(|e| start + e).unwrap();
            &ir[start..end]
        };
        for branch in ["__par_branch_0_0", "__par_branch_0_1"] {
            let frees = fn_body(branch).matches("karac_map_free").count();
            assert_eq!(
                frees, 0,
                "{branch} must not free a slot-published Map handle (ownership \
                 moved to the parent); IR:\n{ir}"
            );
        }
        let main_frees = fn_body("main").matches("karac_map_free").count();
        assert_eq!(
            main_frees, 1,
            "main must free the moved-in Map handle exactly once at scope exit; IR:\n{ir}"
        );
    }

    /// E2E for the same shape: the moved-in Map must be fully usable
    /// after the join (insert + get) and the sibling branch's String
    /// binding must survive to its post-join read. Crash = empty/partial
    /// stdout, so the exact-match assert covers the original SIGSEGV.
    #[test]
    fn test_auto_par_map_slot_use_after_join_e2e() {
        let out = run_program(
            r#"
fn main() {
    let name = "ka" + "ra";
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    m.insert("b", 2);
    let b = m.get("b");
    match b {
        Some(val) => println(val),
        None => println(0),
    }
    println(name);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "2\nkara\n",
                "moved-in Map + sibling String slot must both be live after the join"
            );
        }
    }

    /// Regression (B-2026-06-13-6): a tuple-destructure `let (a, b) = pair()`
    /// whose bindings are read *outside* the auto-par group it lands in. The
    /// return-slot machinery materializes one slot per single-`Binding` `let`,
    /// keyed by `infer_let_binding_llvm_type` (one type per stmt) — it had no
    /// handling for a multi-binding destructure, so `a`/`b` got NO slot, the
    /// destructure was still lifted into a branch fn, and the parent body faulted
    /// with "codegen failed: Undefined variable 'a'" (the panic path in
    /// `run_program`). Fix: `compute_return_slots_checked` now bails the group to
    /// sequential when a destructure-let binding escapes (correctness over a
    /// marginal parallelization — slotting destructure bindings is a future
    /// slice). The annotated `let mut v: Vec[String]` is what forms the group;
    /// `v.push(a)` is the escaping read. A clean run (output matches the
    /// sequential semantics) means codegen no longer aborts.
    #[test]
    fn test_auto_par_tuple_destructure_binding_escapes_group_e2e() {
        let out = run_program(
            r#"
fn pair() -> (String, String) { (f"L", f"R") }
fn main() {
    let mut v: Vec[String] = Vec.new();
    let (a, b) = pair();
    v.push(a);
    println(b);
    println(v[0]);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "R\nL\n",
                "tuple-destructure binding escaping an auto-par group must stay defined"
            );
        }
    }

    /// Regression: an auto-par branch statement that reads an outer local
    /// *inside an `unsafe { }` block* must include that local in the branch's
    /// capture set. The capture-set collector (`refs_in_expr`) previously had
    /// no `ExprKind::Unsafe` arm — it hit the catch-all `_ => {}` and never
    /// saw `m` inside `unsafe { free(m) }`, so the env struct omitted `m` and
    /// the branch fn faulted with "codegen failed: Undefined variable 'm'"
    /// (the panic path in `run_program`). This is exactly the LLVM-C handle
    /// shape — an FFI `*mut` created, let-bound, then passed to a later FFI
    /// call inside `unsafe { }`. `malloc`/`free` (libc) stand in. The fix
    /// aligned `refs_in_expr` with the concurrency analyzer's
    /// `collect_expr_reads`, which already recurses into `Unsafe`/`Try`/
    /// `Par`/`Lock`. A successful run (any stdout, including empty) means
    /// codegen no longer aborts; a regression re-introduces the panic.
    #[test]
    fn test_auto_par_branch_captures_local_read_inside_unsafe_block_e2e() {
        let out = run_program(
            r#"
unsafe extern "C" {
    fn malloc(n: i64) -> *mut u8;
    fn free(p: *mut u8);
}
fn main() {
    let m = unsafe { malloc(8) };
    unsafe { free(m) };
    println("ok");
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(
                out, "ok\n",
                "an auto-par branch reading an FFI handle inside `unsafe {{ }}` must \
                 capture it — the program must compile and print `ok`"
            );
        }
    }

    #[test]
    fn test_auto_par_sort_by_comparator_no_stray_cancel_flag_e2e() {
        // Regression for B-2026-06-18-10. When the auto-par pass parallelizes a
        // function that calls `Vec.sort_by(|a, b| a.cmp(b))`, the sort helper
        // functions (the mono insertion-sort routine and the comparator thunk)
        // were emitted while `branch_cancel_ptr` still pointed at the enclosing
        // par-branch fn's `cancel_flag` argument. The comparator's `a.cmp(b)`
        // method call then ran `emit_branch_cancel_check`, emitting a
        // `load i8, ptr %1` cancel-flag read — but `%1` in those helper
        // functions is an element pointer / the i64 length, not a cancel flag,
        // so LLVM module verification rejected it ("Referring to an argument in
        // another function" + a void/i64 return-type mismatch). The kata #39
        // sorted solver hit this: sort the candidates, then `path.clone()` later
        // in the same function tipped the auto-par pass into parallelizing.
        //
        // The fix clears `branch_cancel_ptr` for the duration of each sort-helper
        // / closure body. The trigger needs the auto-par pass to actually fire:
        // the sort of `v` and the independent build+clone of `p` are two
        // resource-disjoint groups, so the analyzer parallelizes them — which is
        // what placed the comparator emission inside a par-branch cancel context.
        // (A clone of the just-sorted vec instead would chain on the sort and NOT
        // parallelize, so it would not reproduce.) Pre-fix this panicked the
        // codegen with the module-verification error; it must now build and print
        // the combined result.
        let out = run_program(
            r#"
fn solve(input: Slice[i64]) -> i64 {
    let mut v: Vec[i64] = Vec.from_slice(input);
    v.sort_by(|a, b| a.cmp(b));
    let mut p: Vec[i64] = Vec.new();
    p.push(10i64);
    p.push(20i64);
    let q = p.clone();
    v[0] + q[0] + q[1]
}
fn main() {
    // sorted [1,2,3,5,8] -> v[0]=1; q=[10,20]; 1 + 10 + 20 = 31.
    println(f"{solve([5, 3, 8, 1, 2])}");
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out, "31\n");
        }
    }

    // ── B-2026-07-02-31: explicit-par join loses branch bindings when a
    //    branch body is more than a bare call chain ──────────────────────
    //
    // Pre-fix, `infer_let_binding_llvm_type` could not size the return
    // slot for a branch whose `let` RHS lowered to something other than a
    // bare free-function call / identifier alias / literal — notably the
    // operator dispatch (`a + b` → `i64.add(a, b)` after `lower`) and a
    // block-expr RHS (`let y = { ...; tail }`). The slot was dropped, and
    // the join expression's read of that name failed codegen with
    // "Undefined variable". `karac run` (interpreter) was unaffected.
    // These E2E tests build + run each shape and assert the value, so a
    // regression re-introducing the slot-drop fails the build (panics in
    // `run_program`'s `compile_to_object`), not just the assertion.

    /// Repro (a): a branch reads an OUTER (pre-par) binding in an
    /// arithmetic RHS. `base + 1` / `base + 2` lower to `i64.add`
    /// calls; both `x` and `y` slots must be sized so the join `x + y`
    /// resolves. Expected 203.
    #[test]
    fn test_e2e_par_join_branch_reads_outer_binding_arith() {
        let out = run_program(
            r#"
fn main() {
    let base = 100;
    let total = par {
        let x = base + 1;
        let y = base + 2;
        x + y
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "203", "got {out:?}");
        }
    }

    /// Repro (b): a branch's `let` RHS is a nested block expression.
    /// `let y = { let z = 10; z + 1 }` — the block tail `z + 1` must be
    /// walked (with the block-local `z` in scope) to size the `y` slot.
    /// Expected 12.
    #[test]
    fn test_e2e_par_join_branch_rhs_nested_block() {
        let out = run_program(
            r#"
fn main() {
    let t = par {
        let x = 1;
        let y = { let z = 10; z + 1 };
        x + y
    };
    println(t);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "12", "got {out:?}");
        }
    }

    /// Variant: a branch reads TWO outer bindings across arithmetic.
    /// `a + b` and `a * b` both lower to operator calls whose operands
    /// are captures. Expected (10+20)+(10*20) = 230.
    #[test]
    fn test_e2e_par_join_branch_reads_two_outer_bindings() {
        let out = run_program(
            r#"
fn main() {
    let a = 10;
    let b = 20;
    let total = par {
        let x = a + b;
        let y = a * b;
        x + y
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "230", "got {out:?}");
        }
    }

    /// Variant: BOTH branches' RHS are nested blocks. Exercises the
    /// block-tail inference on two distinct slots. Expected (3+4)+(5+6) = 18.
    #[test]
    fn test_e2e_par_join_two_nested_block_branches() {
        let out = run_program(
            r#"
fn main() {
    let t = par {
        let x = { let p = 3; p + 4 };
        let y = { let q = 5; q + 6 };
        x + y
    };
    println(t);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "18", "got {out:?}");
        }
    }

    /// Variant: a comparison branch produces a BOOL slot (`base > 3` →
    /// `i64.gt`, result bool). The join consumes it in an `if`. Confirms
    /// the operator-dispatch inference maps comparison ops to `bool`, not
    /// the operand type. Expected 6 (base+1 with base=5).
    #[test]
    fn test_e2e_par_join_branch_comparison_bool_slot() {
        let out = run_program(
            r#"
fn main() {
    let base = 5;
    let r = par {
        let x = base + 1;
        let y = base > 3;
        if y { x } else { 0 }
    };
    println(r);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "6", "got {out:?}");
        }
    }

    /// Variant: float arithmetic branches — the operator-dispatch
    /// inference must map `f64.add` / `f64.mul` to the f64 slot type, not
    /// the i64 default. Expected (2.5+1.0)+(2.5*2.0) = 8.5.
    #[test]
    fn test_e2e_par_join_branch_float_arith() {
        let out = run_program(
            r#"
fn main() {
    let base = 2.5;
    let total = par {
        let x = base + 1.0;
        let y = base * 2.0;
        x + y
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "8.5", "got {out:?}");
        }
    }

    /// Variant: a branch's `let` RHS is a `match` expression. The
    /// slot type is inferred from a match arm's body. Expected
    /// (match 2 => 20) + (2+5) = 27.
    #[test]
    fn test_e2e_par_join_branch_rhs_match_expr() {
        let out = run_program(
            r#"
fn main() {
    let base = 2;
    let total = par {
        let x = match base { 1 => 10, 2 => 20, _ => 30 };
        let y = base + 5;
        x + y
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "27", "got {out:?}");
        }
    }

    /// Guard: the PLAIN shape (tuple join of bare-call branches) that
    /// already worked must keep working after the branch-shape coverage
    /// widened. Expected 33.
    #[test]
    fn test_e2e_par_join_plain_tuple_of_bare_calls_still_works() {
        let out = run_program(
            r#"
fn f1() -> i64 { 11 }
fn f2() -> i64 { 22 }
fn main() {
    let (p, q) = par {
        let p = f1();
        let q = f2();
        (p, q)
    };
    println(p + q);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "33", "got {out:?}");
        }
    }

    // ── B-2026-07-02-38: residual of B-2026-07-02-31 — a par branch whose
    //    `let` RHS is a METHOD CALL, an INDEX, or a FIELD ACCESS ───────────
    //
    // B-31 (24b1c9f4/ec5c9f20) taught `infer_expr_llvm_type` to size the
    // return slot for operator-dispatch / block / if / match RHS, but it
    // still returned `None` for `ExprKind::{MethodCall, Index, FieldAccess}`,
    // so a branch `let x = base.abs()` / `let x = v[0]` / `let x = p.field`
    // dropped its slot and the join's read of that name failed the *build*
    // with "Undefined variable" while `karac run` executed correctly. Each
    // test below builds + runs the shape and asserts the value, so a
    // regression re-dropping the slot panics in `run_program`'s
    // `compile_to_object`, not just the assertion. All were manually A/B'd
    // under DEFAULT auto-par and `KARAC_AUTO_PAR=0`.

    /// Repro (a): a branch's `let` RHS is a scalar builtin METHOD CALL on an
    /// outer binding. `base.abs()` types as `-> Self` (i64), so the `x` slot
    /// must be sized i64 for the join `x + y`. Expected `7 + 93 = 100`.
    #[test]
    fn test_e2e_par_join_branch_method_call_abs() {
        let out = run_program(
            r#"
fn main() {
    let base = -7;
    let total = par {
        let x = base.abs();
        let y = base + 100;
        x + y
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "100", "got {out:?}");
        }
    }

    /// Repro (b): a branch's `let` RHS is an INDEX read of an outer `Vec[i64]`.
    /// `v[0]` sizes its slot to the Vec's element type (i64, via
    /// `vec_elem_types`). Only the `x` branch touches `v` (the sibling reads a
    /// Copy scalar) so the program is ownership-clean — a plain `Vec` read
    /// from two concurrent branches is a legitimate `ConcurrentPlain*`
    /// diagnostic, orthogonal to the slot-sizing under test. Expected
    /// `5 + 100 = 105`.
    #[test]
    fn test_e2e_par_join_branch_index_read() {
        let out = run_program(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(5);
    v.push(6);
    v.push(7);
    let total = par {
        let x = v[0];
        let y = 100;
        x + y
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "105", "got {out:?}");
        }
    }

    /// Variant: a branch's `let` RHS is a USER IMPL METHOD returning i64. The
    /// `a` slot is sized from the `Point.magsq` LLVM function's declared
    /// return type. Only the `a` branch touches `p` (ownership-clean, per the
    /// index test's note). Expected `(3*3 + 4*4) + 100 = 25 + 100 = 125`.
    #[test]
    fn test_e2e_par_join_branch_user_impl_method() {
        let out = run_program(
            r#"
struct Point { x: i64, y: i64 }
impl Point {
    fn magsq(self) -> i64 { self.x * self.x + self.y * self.y }
}
fn main() {
    let p = Point { x: 3, y: 4 };
    let total = par {
        let a = p.magsq();
        let b = 100;
        a + b
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "125", "got {out:?}");
        }
    }

    /// Variant: a branch's `let` RHS is a struct FIELD ACCESS. The `a` slot is
    /// sized from `Point`'s field `TypeExpr` (i64). Only the `a` branch
    /// touches `p` (ownership-clean, per the index test's note). Expected
    /// `10 + 5 = 15`.
    #[test]
    fn test_e2e_par_join_branch_field_access() {
        let out = run_program(
            r#"
struct Point { x: i64, y: i64 }
fn main() {
    let p = Point { x: 10, y: 20 };
    let total = par {
        let a = p.x;
        let b = 5;
        a + b
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "15", "got {out:?}");
        }
    }

    /// Variant: a NARROW-int scalar-method receiver. `base: i32` with
    /// `base.abs()` — the slot is sized from the receiver's INFERRED LLVM
    /// type, which is i64 (codegen widens every integer local to i64 in
    /// storage and value flow), so the `i64/float` guard resolves it and the
    /// source-level `i32` annotation doesn't leave the slot un-sized.
    /// Expected `7 + 93 = 100`.
    #[test]
    fn test_e2e_par_join_branch_narrow_int_method() {
        let out = run_program(
            r#"
fn main() {
    let base: i32 = -7;
    let total = par {
        let x = base.abs();
        let y = base + 100;
        x + y
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "100", "got {out:?}");
        }
    }

    /// Variant: a FLOAT scalar-method branch. `base.sqrt()` types as `-> Self`
    /// (f64), so the `a` slot must be sized f64, not the i64 default.
    /// Expected `3.0 + 10.0 = 13`.
    #[test]
    fn test_e2e_par_join_branch_float_method() {
        let out = run_program(
            r#"
fn main() {
    let base = 9.0;
    let total = par {
        let a = base.sqrt();
        let b = base + 1.0;
        a + b
    };
    println(total);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "13", "got {out:?}");
        }
    }

    #[test]
    fn test_e2e_a2b2_network_fanout_runs() {
        // A2b-2: two independent `reads(Network) suspends` calls (arg-free →
        // arg-safe) are grouped by auto-par and fanned out. Pins that the group
        // codegen EMITS + RUNS correctly — the analysis-side relaxation
        // (`is_safe_network_fanout`) produces a group the auto-par lowering can
        // fork/join. Output must match sequential execution (ordered-output
        // capture). Companion to the `tests/concurrency.rs` analysis pins and
        // the memory_sanitizer owned-heap variant; runs even where ASAN is
        // unavailable.
        let out = run_program(
            r#"
fn fetch_a() -> i64 with reads(Network) suspends { return 11; }
fn fetch_b() -> i64 with reads(Network) suspends { return 22; }
fn main() {
    let x = fetch_a();
    let y = fetch_b();
    println(x);
    println(y);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "11\n22", "got {out:?}");
        }
    }

    #[test]
    fn test_e2e_a2b2_ephemeral_send_recv_fanout_runs() {
        // A2b-2 Phase 1: two *ephemeral* network calls that `sends(Network)`
        // AND `receives(Network)` — the real `http_get("a"); http_get("b")`
        // shape (owned `String` param, literal arg), NOT the synthetic
        // `reads(Network)` one — now group and fan out. Before Phase 1 the
        // send/recv conflict kept this exact shape serial; now the ephemeral
        // relaxation groups it. Pins that the newly-formed group codegens +
        // runs and that the ordered-output capture keeps stdout byte-identical
        // to sequential execution. The owned-String-param + literal-arg shape
        // here is the one the memory_sanitizer variant proves double-free-clean
        // (the coroutine owns and drops the moved-in `String` exactly once).
        let out = run_program(
            r#"
fn get_a(u: String) -> i64 with sends(Network) receives(Network) { return 11; }
fn get_b(u: String) -> i64 with sends(Network) receives(Network) { return 22; }
fn main() {
    let x = get_a("http://a");
    let y = get_b("http://b");
    println(x);
    println(y);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "11\n22", "got {out:?}");
        }
    }

    #[test]
    fn test_e2e_a2b2_associated_network_opener_fanout_runs() {
        // A2b-2 Phase 2 Slice 1: two *associated* (receiver-less) network
        // openers — the `TcpStream.connect("a"); TcpStream.connect("b")` shape,
        // a 2-segment `Type.method(...)` call with no `self`, owned `String`
        // param, literal arg, real `sends(Network) receives(Network)` — now
        // group and fan out (Phase 1 only reached bare free fns). Pins that the
        // associated-call group codegens + runs and that ordered-output capture
        // keeps stdout byte-identical to sequential execution.
        let out = run_program(
            r#"
struct Net { id: i64 }
impl Net {
    fn open(u: String) -> i64 with sends(Network) receives(Network) { return 33; }
}
fn main() {
    let x = Net.open("host-a");
    let y = Net.open("host-b");
    println(x);
    println(y);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "33\n33", "got {out:?}");
        }
    }

    #[test]
    fn test_e2e_a2b2_method_distinct_receivers_fanout_runs() {
        // A2b-2 Phase 2 Slice 2: two `mut ref self` network method calls on
        // DISTINCT non-shared local receivers fan out and run correctly. The
        // receivers aren't observed after the calls (so the group survives the
        // captured-mutation gate), and each call's return value flows back
        // through its own return slot — output byte-identical to sequential.
        let out = run_program(
            r#"
struct Stream { n: i64 }
impl Stream {
    fn fetch(mut ref self) -> i64 with sends(Network) receives(Network) {
        self.n = self.n + 5;
        return self.n;
    }
}
fn main() {
    let mut s1 = Stream { n: 10 };
    let mut s2 = Stream { n: 20 };
    let a = s1.fetch();
    let b = s2.fetch();
    println(a);
    println(b);
}
"#,
        );
        if let Some(out) = out {
            assert_eq!(out.trim(), "15\n25", "got {out:?}");
        }
    }
}
