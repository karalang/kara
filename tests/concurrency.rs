// tests/concurrency.rs

use karac::concurrency::*;
use karac::{concurrency_analyze, effectcheck, parse};

// ── Test Helpers ────────────────────────────────────────────────

fn analyze(source: &str) -> ConcurrencyAnalysis {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let effects = effectcheck(&parsed.program);
    concurrency_analyze(&parsed.program, &effects)
}

fn get_function<'a>(analysis: &'a ConcurrencyAnalysis, name: &str) -> &'a FunctionConcurrency {
    analysis
        .function_decisions
        .get(name)
        .unwrap_or_else(|| panic!("function '{}' not found in analysis", name))
}

// ── Pure independent calls are parallelizable ──────────────────

#[test]
fn test_pure_independent_calls() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            let x = a();
            let y = b();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // Both statements should be in a single parallel group
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
    assert!(main_fc.parallel_groups[0].statement_indices.contains(&0));
    assert!(main_fc.parallel_groups[0].statement_indices.contains(&1));
}

// ── Data dependency forces serialization ───────────────────────

#[test]
fn test_data_dependency_serializes() {
    let analysis = analyze(
        r#"
        fn main() {
            let x = 1;
            let y = x + 1;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // No parallel groups because y depends on x
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to data dependency, got {:?}",
        main_fc.parallel_groups
    );
}

// ── Effect conflict forces serialization ───────────────────────

#[test]
fn test_effect_conflict_serializes() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read_db() reads(Db) { }
        fn write_db() writes(Db) { }
        fn main() {
            read_db();
            write_db();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // reads + writes on same resource = conflict, no parallel group
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to effect conflict (reads+writes on Db), got {:?}",
        main_fc.parallel_groups
    );
}

// ── Different resources are parallelizable ─────────────────────

#[test]
fn test_different_resources_parallelizable() {
    let analysis = analyze(
        r#"
        effect resource Db;
        effect resource Cache;
        fn read_db() reads(Db) { }
        fn read_cache() reads(Cache) { }
        fn main() {
            read_db();
            read_cache();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

// ── reads+reads on same resource is safe ───────────────────────

#[test]
fn test_reads_reads_same_resource_safe() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read1() reads(Db) { }
        fn read2() reads(Db) { }
        fn main() {
            read1();
            read2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

// ── writes+writes on same resource conflicts ───────────────────

#[test]
fn test_writes_writes_same_resource_conflicts() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn write1() writes(Db) { }
        fn write2() writes(Db) { }
        fn main() {
            write1();
            write2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to writes+writes conflict, got {:?}",
        main_fc.parallel_groups
    );
}

// ── seq {} forces sequential ───────────────────────────────────

#[test]
fn test_seq_forces_sequential() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            seq {
                let x = a();
                let y = b();
            };
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    // The seq block is a single Expr statement wrapping the block
    // The inner statements within seq are forced sequential
    // but main only has 1 top-level statement (the seq expression)
    assert_eq!(main_fc.total_statements, 1);
    // With only 1 statement, no parallel groups possible
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Cross-category effects don't conflict ──────────────────────

#[test]
fn test_cross_category_no_conflict() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read_db() reads(Db) { }
        fn send_db() sends(Db) { }
        fn main() {
            read_db();
            send_db();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // reads + sends = different categories = no conflict
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

// ── sends+sends on same resource conflicts ─────────────────────

#[test]
fn test_sends_sends_same_resource_conflicts() {
    let analysis = analyze(
        r#"
        effect resource Chan;
        fn send1() sends(Chan) { }
        fn send2() sends(Chan) { }
        fn main() {
            send1();
            send2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to sends+sends conflict"
    );
}

// ── Empty function ─────────────────────────────────────────────

#[test]
fn test_empty_function() {
    let analysis = analyze("fn main() { }");
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 0);
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Single statement — no parallelism possible ─────────────────

#[test]
fn test_single_statement() {
    let analysis = analyze(
        r#"
        fn main() {
            let x = 1;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 1);
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Multiple independent pure statements ───────────────────────

#[test]
fn test_multiple_independent_pure() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = 2;
            let c = 3;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 3);
}

// ── Chain dependency: a -> b -> c ──────────────────────────────

#[test]
fn test_chain_dependency() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = a + 1;
            let c = b + 1;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    // `a` and `c` have no direct edge between them (c reads b, not a),
    // but `b` sits between them with a hard dep on both. The
    // contiguous-only grouping rule rejects [0, 2]: codegen emits a
    // single `karac_par_run` fan-out at the group's min_idx, so
    // skipping over a dependent middle stmt would either drop stmt 1
    // entirely or produce a branch that reads a binding the analyzer
    // can't guarantee is in scope. So no parallel group fires here.
    assert_eq!(main_fc.parallel_groups.len(), 0);
}

// ── Full chain: every statement reads previous ─────────────────

#[test]
fn test_full_chain_no_parallelism() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = a;
            let c = a + b;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    // b reads a, c reads both a and b — no independent pair
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Diamond dependency pattern ─────────────────────────────────

#[test]
fn test_diamond_dependency() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = a + 1;
            let c = a + 2;
            let d = b + c;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 4);
    // b and c both depend on a, but are independent of each other
    // d depends on both b and c
    // So b and c can be parallel
    assert!(!main_fc.parallel_groups.is_empty());
    // Find the group containing b (index 1) and c (index 2)
    let bc_group = main_fc
        .parallel_groups
        .iter()
        .find(|g| g.statement_indices.contains(&1) && g.statement_indices.contains(&2));
    assert!(
        bc_group.is_some(),
        "Expected b and c to be in a parallel group"
    );
}

// ── Transitive effect inheritance ──────────────────────────────

#[test]
fn test_transitive_effect_inheritance() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn helper() writes(Db) { }
        fn wrapper() { helper(); }
        fn reader() reads(Db) { }
        fn main() {
            wrapper();
            reader();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // wrapper() transitively writes(Db), reader() reads(Db) -> conflict
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to transitive effect conflict"
    );
}

// ── Parallel group reason descriptions ─────────────────────────

#[test]
fn test_reason_pure_computations() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            let x = a();
            let y = b();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(!main_fc.parallel_groups.is_empty());
    assert_eq!(main_fc.parallel_groups[0].reason, "pure computations");
}

#[test]
fn test_reason_concurrent_reads() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read1() reads(Db) { }
        fn read2() reads(Db) { }
        fn main() {
            read1();
            read2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(!main_fc.parallel_groups.is_empty());
    assert_eq!(
        main_fc.parallel_groups[0].reason,
        "concurrent reads on same resource"
    );
}

#[test]
fn test_reason_different_resources() {
    let analysis = analyze(
        r#"
        effect resource Db;
        effect resource Cache;
        fn read_db() reads(Db) { }
        fn read_cache() reads(Cache) { }
        fn main() {
            read_db();
            read_cache();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(!main_fc.parallel_groups.is_empty());
    assert_eq!(
        main_fc.parallel_groups[0].reason,
        "independent reads on different resources"
    );
}

// ── Cost-model gate: zero-parallelism shapes are marked trivial ─

#[test]
fn test_cost_model_one_expensive_plus_lets_marked_trivial() {
    // One effectful stmt + N constant-init lets has zero structural
    // parallelism: one par branch holds all the work, the others
    // idle. Pre-fix the analyzer still emitted the group as
    // non-trivial and the codegen paid `karac_par_run` spawn cost
    // (~70μs/dispatch on macOS) for no speedup. Post-fix the
    // cost-model gate routes these through `is_trivial = true` so
    // codegen skips the par dispatch.
    let analysis = analyze(
        r#"
        effect resource R;
        fn worker() writes(R) {}
        fn main() {
            let mut x = 0i64;
            worker();
            let mut y = 0i64;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    let group = &main_fc.parallel_groups[0];
    assert_eq!(group.statement_indices.len(), 3);
    assert!(
        group.is_trivial,
        "Group with 1 effectful stmt + 2 constant-init lets should be \
         marked trivial (zero structural parallelism)"
    );
}

#[test]
fn test_cost_model_hot_loop_plus_let_init_marked_trivial() {
    // Distillation of the kata 6 zigzag failure mode: the analyzer
    // groups a hot push loop with a let-init for the next phase's
    // counter (`let mut r2 = 0i64`). Both stmts are independent
    // (no shared vars, no effect conflict on the loop's
    // `allocates(Heap)`), so the analyzer correctly identifies
    // them as a parallelizable pair — but parallelizing yields
    // no speedup since one branch sits on the let-of-literal and
    // the other does all the work. Drove the kata 6 bench's 2.5×
    // gap vs sequential codegen (2026-05-17).
    let analysis = analyze(
        r#"
        fn main() {
            let mut v: Vec[i64] = Vec.new();
            let mut i = 0i64;
            while i < 10 {
                v.push(i);
                i = i + 1;
            }
            let mut r2 = 0i64;
            let last = v.len() - 1;
            println(v[last]);
            println(r2);
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    // Every group that survives the cost-model gate must have
    // 2+ stmts that do real work. The kata 6 shape only produces
    // "one-big + N-cheap" groups, so all must be trivial.
    for group in &main_fc.parallel_groups {
        assert!(
            group.is_trivial,
            "Group {:?} (reason: {:?}) should be marked trivial — \
             only one of its stmts does meaningful work",
            group.statement_indices, group.reason
        );
    }
}

#[test]
fn test_cost_model_two_effectful_calls_still_parallelized() {
    // Control case: two effectful calls on independent resources
    // have real structural parallelism. The cost-model gate must
    // NOT mark them trivial — codegen should still dispatch the
    // par_run so both calls run concurrently.
    let analysis = analyze(
        r#"
        effect resource R1;
        effect resource R2;
        fn w1() writes(R1) {}
        fn w2() writes(R2) {}
        fn main() {
            w1();
            w2();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    let group = &main_fc.parallel_groups[0];
    assert_eq!(group.statement_indices.len(), 2);
    assert!(
        !group.is_trivial,
        "Two effectful calls on independent resources have real \
         structural parallelism — must not be marked trivial"
    );
}

#[test]
fn test_cost_model_let_with_effectful_rhs_counts_as_work() {
    // Control case: a `let x = call()` stmt where the RHS is a
    // function call (not a literal/identifier) counts as work.
    // Two such lets in a group have real parallelism and must
    // not be filtered out.
    let analysis = analyze(
        r#"
        effect resource R1;
        effect resource R2;
        fn compute1() -> i64 writes(R1) { 0 }
        fn compute2() -> i64 writes(R2) { 0 }
        fn main() {
            let x = compute1();
            let y = compute2();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    let group = &main_fc.parallel_groups[0];
    assert_eq!(group.statement_indices.len(), 2);
    assert!(
        !group.is_trivial,
        "Two let-bindings whose RHS calls effectful functions have \
         work-bearing RHS expressions — must not be marked trivial"
    );
}

// ── CLI query test ─────────────────────────────────────────────

#[test]
fn test_cli_query_concurrency() {
    use std::io::Write;
    use std::process::Command;

    // Write a temp .kara file
    let dir = std::env::temp_dir();
    let file_path = dir.join("test_concurrency_query.kara");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(
            f,
            r#"
fn a() -> i32 {{ 1 }}
fn b() -> i32 {{ 2 }}
fn main() {{
    let x = a();
    let y = b();
}}
"#
        )
        .unwrap();
    }

    // Use the binary that cargo already built for this test run
    let karac_bin = env!("CARGO_BIN_EXE_karac");

    // Run karac query concurrency
    let output = Command::new(karac_bin)
        .args([
            "query",
            "concurrency",
            &format!("{}.main", file_path.display()),
        ])
        .output()
        .expect("failed to run karac");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "karac query concurrency failed: {}{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify JSON output contains expected fields
    assert!(
        stdout.contains("\"function\":\"main\""),
        "stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("\"total_statements\":2"),
        "stdout: {}",
        stdout
    );
    assert!(stdout.contains("\"parallel_groups\""), "stdout: {}", stdout);

    // Clean up
    let _ = std::fs::remove_file(&file_path);
}

// ── Assign-target dependencies ─────────────────────────────────

#[test]
fn test_assign_creates_dependency() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut x = 1;
            x = 2;
            let y = x;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    // All three are linked: x defined, x assigned, x read
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Task granularity heuristics ──────────────────────────────────

#[test]
fn test_pure_group_is_trivial() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            let x = a();
            let y = b();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert!(
        main_fc.parallel_groups[0].is_trivial,
        "pure computation group should be marked trivial"
    );
}

#[test]
fn test_effectful_group_not_trivial() {
    let analysis = analyze(
        r#"
        resource Db;
        fn read_a() -> i32 with reads(Db) { 1 }
        fn read_b() -> i32 with reads(Db) { 2 }
        fn main() with reads(Db) {
            let x = read_a();
            let y = read_b();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert!(
        !main_fc.parallel_groups[0].is_trivial,
        "effectful group should NOT be trivial"
    );
}

// ── Polymorphic-effect calls serialize conservatively ─────────

#[test]
fn test_polymorphic_calls_serialize() {
    // Two calls to a `with _` function have unknown runtime effects and must
    // not be parallelized — they might conflict on shared resources that the
    // inferred-effect set cannot see.
    let analysis = analyze(
        r#"
        effect resource Db;
        pub fn poly() with _ { }
        fn main() {
            let x = poly();
            let y = poly();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert!(
        main_fc.parallel_groups.is_empty(),
        "two polymorphic calls must not be parallelized, got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_polymorphic_and_pure_can_parallelize() {
    // A polymorphic call and a pure computation can still parallelize — the
    // pure statement has no effects to be disturbed by the polymorphic one.
    let analysis = analyze(
        r#"
        effect resource Db;
        pub fn poly() with _ { }
        fn main() {
            let x = poly();
            let y = 1 + 2;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

#[test]
fn test_polymorphic_group_not_trivial() {
    // Even when a group contains two parallelizable statements, if one of them
    // transitively calls a `with _` function, the group cannot be dispatched
    // as trivial — the runtime effects are unknown.
    let analysis = analyze(
        r#"
        effect resource Db;
        pub fn poly() with _ { }
        fn main() {
            let x = poly();
            let y = 1 + 2;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert!(
        !main_fc.parallel_groups[0].is_trivial,
        "group containing a polymorphic call must not be marked trivial"
    );
}
