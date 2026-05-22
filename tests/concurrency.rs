// tests/concurrency.rs

use karac::concurrency::*;
use karac::{concurrency_analyze, effectcheck, lower, parse, resolve, typecheck};

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

/// Mirror of `analyze` that runs the typecheck + lowering passes
/// before concurrency analysis. The CLI pipeline lowers primitive
/// operators into trait-method calls before concurrencycheck runs
/// (`src/lowering.rs`), so the reduction recognizer must handle the
/// post-lowering `Call(Path([type, op_method]), [a, b])` shape as
/// well as the parser-shape `Binary { op, left, right }`. Without
/// this lowered-pipeline test, the kata-7 / Parallax CLI surface
/// would silently regress while the parse-shape unit tests pass.
fn analyze_lowered(source: &str) -> ConcurrencyAnalysis {
    let mut parsed = parse(source);
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
    let resolved = resolve(&parsed.program);
    let tc = typecheck(&parsed.program, &resolved);
    lower(&mut parsed.program, &tc);
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

// ── Reduction recognition (auto-par slice 1, 2026-05-19) ───────
//
// Tests for the loop-reduction recognizer: each top-level `for` / `while` /
// `loop` whose body's only loop-carried write follows `acc = acc <op> expr`
// (or `acc op= expr`) for op ∈ {+, *, |, &, ^} is tagged with a
// `LoopReduction`. Induction-shape writes (`i = i + const_lit`, `i +=
// const_lit`) are folded alongside as loop-counter steps so explicit
// `while` loops match without the reduction being broken by the counter.

#[test]
fn test_reduction_recognized_for_add_while_loop() {
    // The kata-7 bench shape: `while k < K { sum = sum + ...; k = k + 1; }`.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                sum = sum + k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one reduction, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "sum");
    assert_eq!(r.op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_compound_add() {
    // `total += x` shape parses to CompoundAssign — must also be recognized.
    let analysis = analyze(
        r#"
        fn main() {
            let mut total: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                total += k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "total");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_mul_or_and_xor() {
    // Sweep the four other ops in one program — one loop each, four
    // recognized reductions, each tagged with its op.
    let analysis = analyze(
        r#"
        fn main() {
            let mut p: i64 = 1i64;
            let mut a: i64 = 0i64;
            while a < 5i64 {
                p = p * a;
                a = a + 1i64;
            }
            let mut o: i64 = 0i64;
            let mut b: i64 = 0i64;
            while b < 5i64 {
                o = o | b;
                b = b + 1i64;
            }
            let mut n: i64 = -1i64;
            let mut c: i64 = 0i64;
            while c < 5i64 {
                n = n & c;
                c = c + 1i64;
            }
            let mut x: i64 = 0i64;
            let mut d: i64 = 0i64;
            while d < 5i64 {
                x = x ^ d;
                d = d + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 4);
    let by_acc: std::collections::HashMap<_, _> = main_fc
        .loop_reductions
        .iter()
        .map(|r| (r.accumulator.clone(), r.op))
        .collect();
    assert_eq!(by_acc.get("p"), Some(&ReductionOp::Mul));
    assert_eq!(by_acc.get("o"), Some(&ReductionOp::BitOr));
    assert_eq!(by_acc.get("n"), Some(&ReductionOp::BitAnd));
    assert_eq!(by_acc.get("x"), Some(&ReductionOp::BitXor));
}

#[test]
fn test_reduction_commutative_rhs_acc_position() {
    // `sum = k + sum` — accumulator on the right. Allow-list ops are
    // commutative, so this shape is equally valid.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                sum = k + sum;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
}

#[test]
fn test_reduction_rejects_subtraction() {
    // `acc -= x` is NOT associative (a - b - c ≠ a - (b - c)) and not in
    // the allow-list. The classifier must reject the loop.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                acc = acc - k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "subtraction is not associative; should not be tagged as reduction"
    );
}

#[test]
fn test_reduction_rejects_division() {
    // Division is neither associative nor commutative.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 100i64;
            let mut k: i64 = 1i64;
            while k < 5i64 {
                acc /= k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(main_fc.loop_reductions.is_empty());
}

#[test]
fn test_reduction_rejects_multiple_distinct_accumulators() {
    // Two distinct accumulators in the same loop — slice 1 only handles
    // single-accumulator reductions, so the loop is rejected entirely.
    let analysis = analyze(
        r#"
        fn main() {
            let mut a: i64 = 0i64;
            let mut b: i64 = 1i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                a = a + k;
                b = b * k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "two-accumulator loop should not match slice-1 recognition"
    );
}

#[test]
fn test_reduction_recognized_for_nested_conditional_acc_update() {
    // An inner `if cond { acc = acc + k; }` was conservatively rejected
    // under slice 1. After the 2026-05-20 conditional-acc-update slice
    // landed (see [`conditional_acc_update_shape`] in src/concurrency.rs),
    // this shape is recognized: it's semantically equivalent to
    // `acc = acc + (if cond { k } else { 0 })`, the per-iter contribution
    // is order-independent, and `cond` here (`k > 5i64`) doesn't read the
    // accumulator. The "if { acc = ... } rejected" conservative-default
    // tests survive in the rejects-when-cond-reads-acc / rejects-with-
    // nonempty-else / rejects-with-extra-stmt-in-then siblings below.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                if k > 5i64 {
                    acc = acc + k;
                }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "acc");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_loop() {
    // `for k in 0..K { acc = acc + ... }` — no explicit induction, the
    // for-binding is fresh per-iter. Body has a single loop-carried
    // write; recognized cleanly.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 0i64;
            for k in 0..100i64 {
                acc = acc + k;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "acc");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_after_lowering() {
    // CLI surface regression check (slice 1, 2026-05-19): the same
    // shape as `test_reduction_recognized_for_add_while_loop` but run
    // through resolve + typecheck + lower before concurrency. The
    // lowering pass rewrites `sum + k` into a `Call(Path(["i64",
    // "add"]), [sum, k])` shape that the recognizer must also match.
    let analysis = analyze_lowered(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                sum = sum + k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "post-lowering Call shape must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_no_reduction_when_loop_has_no_accumulator() {
    // A loop that only steps the counter — no accumulator, no reduction.
    let analysis = analyze(
        r#"
        fn main() {
            let mut k: i64 = 0i64;
            while k < 100i64 {
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(main_fc.loop_reductions.is_empty());
}

// ── Min/Max recognition (combined slice, 2026-05-20) ────────────────
// Both direct-call (`m = i64.min(m, x)`) and conditional-assign
// (`if x < m { m = x; }`) shapes are recognized as Min/Max reductions
// over a single accumulator. The kata-153 linear_scan bench is the
// validation workload — its `find_min`'s `if x < m { m = x; }` inner
// loop drives the conditional-assign branch.

#[test]
fn test_reduction_recognized_for_conditional_min() {
    // The kata-153 shape: `if x < m { m = x; }` inside an inner loop.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = 1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x < m {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one Min reduction, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Min);
}

#[test]
fn test_reduction_recognized_for_conditional_max() {
    // Mirror of the Min shape: `if x > m { m = x; }`.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = -1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x > m {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Max);
}

#[test]
fn test_reduction_recognized_for_conditional_minmax_commutative() {
    // Commutative form: `if m > x { m = x; }` is Min, `if m < x { m = x; }` is Max.
    let analysis = analyze(
        r#"
        fn main() {
            let mut lo: i64 = 1000i64;
            let mut hi: i64 = -1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if lo > x {
                    lo = x;
                }
                i = i + 1i64;
            }
            let mut j: i64 = 0i64;
            while j < 100i64 {
                let y: i64 = j * 7i64;
                if hi < y {
                    hi = y;
                }
                j = j + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 2);
    let by_acc: std::collections::HashMap<_, _> = main_fc
        .loop_reductions
        .iter()
        .map(|r| (r.accumulator.clone(), r.op))
        .collect();
    assert_eq!(by_acc.get("lo"), Some(&ReductionOp::Min));
    assert_eq!(by_acc.get("hi"), Some(&ReductionOp::Max));
}

#[test]
fn test_reduction_rejects_conditional_with_else_branch() {
    // `if x < m { m = x; } else { m = x + 1; }` is not a clean Min step
    // — the else branch also writes the accumulator, recognition rejects.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = 1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x < m {
                    m = x;
                } else {
                    m = x + 1i64;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "if-with-else should not be Min-recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_conditional_when_cond_unrelated_to_value() {
    // `if y < z { m = x; }` — cond compares y/z but assigns x to m.
    // Doesn't fit the `value < acc` Min shape; no recognition.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = 0i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                let y: i64 = i * 11i64;
                let z: i64 = i * 13i64;
                if y < z {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(main_fc.loop_reductions.is_empty());
}

#[test]
fn test_reduction_recognized_for_conditional_min_after_lowering() {
    // End-to-end through the lowering pipeline — the cond's `<` becomes
    // a `Call(Path(["i64", "lt"]), [a, b])` shape; recognition handles both.
    let analysis = analyze_lowered(
        r#"
        fn main() {
            let mut m: i64 = 1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x < m {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "post-lowering Call(lt) shape must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Min);
}

#[test]
fn test_reduction_recognized_for_conditional_min_for_range_after_lowering() {
    // kata-153 find_min shape: `for i in 1..n { let x = nums[i]; if x < m { m = x; } }`.
    // Same conditional-assign pattern as the while form, but inside an
    // impl-level free function with a Slice parameter and the for-range
    // loop construct. Validates the analyzer recognizes the kata's
    // actual shape end-to-end through the CLI pipeline.
    let analysis = analyze_lowered(
        r#"
        fn find_min(nums: Slice[i64]) -> i64 {
            let n = nums.len();
            let mut m = nums[0];
            for i in 1..n {
                let x = nums[i];
                if x < m {
                    m = x;
                }
            }
            m
        }
        fn main() { }
        "#,
    );
    let find_min_fc = get_function(&analysis, "find_min");
    assert_eq!(
        find_min_fc.loop_reductions.len(),
        1,
        "expected one Min reduction on `m`, got {:?}",
        find_min_fc.loop_reductions
    );
    assert_eq!(find_min_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(find_min_fc.loop_reductions[0].op, ReductionOp::Min);
}

// ── Conditional accumulator-update recognition (slice: conditional-acc-update, 2026-05-20) ──
// `if cond { acc = acc + delta; }` (and `if cond { acc OP= delta; }`)
// is semantically equivalent to `acc = acc + (if cond { delta } else { 0 })`
// for any associative+commutative op with a known identity, so the
// pattern is a reduction step. Surfaced by kata-65 bench (count of
// truthy `is_number(...)` results); pre-fix the analyzer reported
// `<no parallelization opportunities detected>` and the workload ran
// single-threaded (User ≈ wall, no parallelism).

#[test]
fn test_reduction_recognized_for_conditional_acc_update_assign() {
    // The kata-65 bench shape: `if cond { sum = sum + 1i64; }`.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum = sum + 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one reduction, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "sum");
    assert_eq!(r.op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_conditional_acc_update_compound() {
    // CompoundAssign form: `if cond { sum += 1i64; }`. The unconditional
    // `acc += const_lit` is reserved as the loop-counter shape; under
    // a conditional wrap the matcher recognizes it as the "count of
    // truthy iterations" reduction.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum += 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_conditional_acc_update_with_empty_else() {
    // `if cond { acc = acc + 1 } else { }` — explicit empty else parses
    // as an If with else_branch = Some(Block{stmts:[], final_expr:None})
    // and is semantically identical to the no-else form.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum = sum + 1i64; } else { }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_conditional_acc_update_mul() {
    // Non-Add op variant — `if cond { p = p * 2i64; }` over a Mul accumulator.
    let analysis = analyze(
        r#"
        fn main() {
            let mut p: i64 = 1i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                let cond: bool = k > 2i64;
                if cond { p = p * 2i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "p");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Mul);
}

#[test]
fn test_reduction_rejects_conditional_acc_update_when_cond_reads_acc() {
    // `if sum > 100 { sum = sum + 1 }` — the condition reads the
    // accumulator, so the per-iter decision depends on accumulator
    // state from earlier iterations. Not order-independent; reject.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                if sum > 100i64 { sum = sum + 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "cond reading acc must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_recognized_for_two_arm_acc_update_same_op() {
    // 2026-05-20 slice extension: `if cond { acc = acc + a } else { acc
    // = acc + b }` is semantically equivalent to `acc = acc + (if cond
    // { a } else { b })` and recognizable as a `+` reduction. The
    // matcher accepts when both arms target the same accumulator with
    // the same op; mixed accumulators or mixed ops are rejected
    // (see siblings below).
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum = sum + 1i64; } else { sum = sum + 2i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "two-arm same-acc same-op must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "sum");
    assert_eq!(r.op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_two_arm_acc_update_compound() {
    // CompoundAssign in both arms with different deltas — same shape as
    // the canonical "hit/miss tally" workload (e.g., `if hit { right +=
    // 1 } else { right += 0 }` — though if both arms had identical
    // deltas the unconditional form would be simpler).
    let analysis = analyze(
        r#"
        fn main() {
            let mut tally: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let hit: bool = (k % 3i64) == 0i64;
                if hit { tally += 3i64; } else { tally += 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "tally");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_two_arm_acc_update_with_variable_deltas() {
    // Both arms have non-literal delta expressions. The
    // `reduction_binary_shape` machinery still requires acc to appear
    // exactly once on each RHS, so non-acc operands are acc-free by
    // construction — both arms recognize as `+`-step contributions.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let even: bool = (k % 2i64) == 0i64;
                if even { sum = sum + (k * 2i64); } else { sum = sum + k; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_rejects_two_arm_acc_update_with_different_accumulators() {
    // `if cond { a = a + 1 } else { b = b + 1 }` — different
    // accumulators per branch. Each arm IS a valid 1-arm shape on its
    // own, but the if-block as a whole writes two distinct names and
    // doesn't fit the single-accumulator fan-out model.
    let analysis = analyze(
        r#"
        fn main() {
            let mut a: i64 = 0i64;
            let mut b: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { a = a + 1i64; } else { b = b + 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "mixed-accumulator two-arm must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_two_arm_acc_update_with_mixed_ops() {
    // `if cond { acc = acc + 1 } else { acc = acc * 2 }` — same acc,
    // but different ops. The fan-out + combine model commutes only
    // within a single op, so the contribution-as-`+` and contribution-
    // as-`*` forms can't be unified into one reduction.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 1i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { acc = acc + 1i64; } else { acc = acc * 2i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "mixed-op two-arm must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_conditional_acc_update_with_extra_stmt_in_then() {
    // Then-block has two stmts — not the single-stmt shape the
    // recognizer accepts. The trailing `let local = ...` doesn't
    // touch the accumulator but the shape constraint still rejects.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond {
                    sum = sum + 1i64;
                    let _local: i64 = k * 2i64;
                }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "multi-stmt then-block must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

// ── Collect-style reduction recognition (slice: par-unordered Phase 2, 2026-05-20) ──
// `#[par_unordered] while ... { ...acc.push(x)... }` — the analyzer
// recognizes `acc.push(x)` (bare) and `if cond { acc.push(x); }`
// (conditional) shapes as `ReductionOp::Collect` only when the
// enclosing loop carries the `#[par_unordered]` attribute. Without the
// opt-in, the same shape falls through to "no parallelization
// opportunities detected" because per-worker partial-Vec concat
// produces worker-order output, not iteration-order — a semantic
// surprise the user must opt into explicitly.

#[test]
fn test_reduction_recognized_for_bare_push_when_par_unordered() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                results.push(k);
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "bare push with par_unordered must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "results");
    assert_eq!(r.op, ReductionOp::Collect);
}

#[test]
fn test_reduction_recognized_for_conditional_push_when_par_unordered() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                if k > 5i64 {
                    results.push(k);
                }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "conditional push with par_unordered must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "results");
    assert_eq!(r.op, ReductionOp::Collect);
}

#[test]
fn test_reduction_recognized_for_conditional_push_with_empty_else() {
    // Empty else passes through the same matcher path as the no-else
    // case (mirror of conditional_acc_update_shape's empty-else
    // acceptance).
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                if k > 5i64 { results.push(k); } else { }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "results");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Collect);
}

#[test]
fn test_reduction_rejects_bare_push_without_par_unordered() {
    // Same source as the bare-push-recognized test above but with the
    // attribute removed — the same `results.push(k)` body that
    // *would* be recognized under opt-in must fall through to "no
    // reduction" without the attribute. This is the key safety
    // property: collect-style auto-par requires explicit user opt-in.
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            while k < 100i64 {
                results.push(k);
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "bare push without par_unordered must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_conditional_push_without_par_unordered() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            while k < 100i64 {
                if k > 5i64 { results.push(k); }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "conditional push without par_unordered must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_push_on_let_introduced_acc() {
    // `let mut local: Vec[i64] = Vec.new();` *inside* the loop body
    // creates a body-local accumulator — pushing into it isn't loop-
    // carried; same shape that's already rejected for scalar
    // reductions (see `test_reduction_recognized_for_two_arm_acc_update_same_op`'s
    // `let_introduced` guard). Even with the par_unordered opt-in,
    // body-local accumulators don't fan out across workers.
    let analysis = analyze(
        r#"
        fn main() {
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                let mut local: Vec[i64] = Vec.new();
                local.push(k);
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "body-local push must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_mixed_push_and_scalar_accumulator() {
    // Two distinct accumulators per iter — one Collect, one Add. The
    // single-accumulator contract is preserved: the matcher returns
    // None when reductions of different kinds appear in the same loop.
    // The scalar accumulator uses `total = total + k` (sum of loop
    // indices) rather than `total += 1i64`, because the `acc + const_lit`
    // form is special-cased upstream as the loop-counter (induction-step)
    // shape and is *ignored* by the matcher rather than treated as a
    // competing reduction — that case doesn't actually exercise the
    // mixed-acc rejection path.
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut total: i64 = 0i64;
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                results.push(k);
                total = total + k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "mixed Collect+Add must not be recognized as a single reduction, got {:?}",
        main_fc.loop_reductions
    );
}

// ── Nested-binary chain recognition (slice: chain recognizer, 2026-05-20) ──
// `sum = sum + a + b` parses left-associatively as
// `Binary(+, Binary(+, sum, a), b)`. Today's `reduction_binary_shape`
// only checks the outer Binary's direct operands for the acc identifier
// — neither child of the outer matches, so the chain falls through to
// "rejected." Slice extends recognition: count acc occurrences across
// the same-op chain; recognize iff acc appears exactly once.

#[test]
fn test_reduction_recognized_for_chain_of_two() {
    // `sum = sum + a + b` — kata-5-outer shape.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = sum + a + b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one Add reduction on `sum`, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_chain_of_four() {
    // Longer chain `sum + a + b + c + d` — pins that the chain walker
    // recursion handles arbitrary depth, not just 2 levels.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                let c: i64 = k * 5i64;
                let d: i64 = k * 7i64;
                sum = sum + a + b + c + d;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_chain_with_acc_in_middle() {
    // `a + sum + b` parses as `Binary(+, Binary(+, a, sum), b)`.
    // Commutativity makes this equivalent to `sum + a + b`; the chain
    // walker counts occurrences without caring about position.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = a + sum + b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_chain_does_not_recurse_into_mixed_ops() {
    // `sum + a * b` — outer is `+`, inner is `*`. The walker stops at
    // the inner `*` (not same op as target `+`), treating `a * b` as a
    // single leaf. `sum + (a * b)` matches the direct `acc + expr`
    // shape, so the reduction IS recognized — and correctly so, since
    // `(a * b)` is just an opaque value combined with acc once per iter.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = sum + a * b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "outer `acc + (a*b)` should match direct shape"
    );
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_rejects_chain_with_acc_twice() {
    // `sum + sum + a` — acc appears twice in the chain. NOT a valid
    // reduction step: per-iter combine `sum := sum + sum + a` is
    // `2*sum + a`, but partials initialized to identity (0) wouldn't
    // compose correctly under the standard fan-out + Add-combine model.
    // Chain walker counts acc=2; recognizer rejects.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k;
                sum = sum + sum + a;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "double-acc chain should reject; got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_recognized_for_chain_after_lowering() {
    // End-to-end through the lowering pipeline — the chain becomes
    // `Call(Path([T, "add"]), [Call(Path([T, "add"]), [sum, a]), b])`.
    // Chain walker recurses through the Call shape too.
    let analysis = analyze_lowered(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = sum + a + b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "post-lowering chain Call shape must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}
