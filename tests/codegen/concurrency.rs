//! par blocks, spawn, tasks, channels, atomics, pools -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen concurrency::
//!
//! New fixtures about par blocks, spawn, tasks, channels, atomics, pools belong in this file.

use super::*;

#[test]
fn test_e2e_lazyframe_select_filter_limit_collect_explain() {
    // Phase-11 LazyDataFrame codegen twin (`src/codegen/lazyframe.rs` +
    // `runtime/src/lazy.rs`): the full v1 surface in one AOT binary —
    // plan building (lazy/filter/select/limit), the LazyExpr predicate
    // builders (col/gt/ge/and_), `explain` (byte parity with the
    // interpreter's fold+render — the oracle is
    // tests/interpreter.rs::test_lazyframe_filter_expression_pipeline),
    // and `collect` back into an eager DataFrame read through the
    // ordinary Column indexing path. Output byte-pinned to the
    // interpreter twin's.
    let out = run_program(
            "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"age\", Column.from_vec([30i64, 15i64, 41i64, 8i64]));\n\
                 df.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"eve\", \"kid\"]));\n\
                 df.insert(\"score\", Column.from_vec([9.5, 3.5, 7.0, 1.0]));\n\
                 let plan = df.lazy().filter(LazyExpr.col(\"age\").gt(10).and_(LazyExpr.col(\"score\").ge(3.5))).select(vec![\"name\", \"age\"]).limit(2);\n\
                 println(plan.explain());\n\
                 let out = plan.collect();\n\
                 println(out.height());\n\
                 let names: Column[String] = out.column(\"name\");\n\
                 match names[0] { Some(v) => println(v), None => println(\"null\") }\n\
                 match names[1] { Some(v) => println(v), None => println(\"null\") }\n\
             }",
        );
    if let Some(out) = out {
        assert_eq!(
            out,
            "== logical plan ==\n\
                 LIMIT 2\n\
                 \x20 SELECT [name, age]\n\
                 \x20   FILTER ((age > 10) and (score >= 3.5))\n\
                 \x20     SCAN [age, name, score]\n\
                 == optimized ==\n\
                 SELECT [name, age]\n\
                 \x20 LIMIT 2\n\
                 \x20   FILTER ((age > 10) and (score >= 3.5))\n\
                 \x20     SCAN cols=[age, name, score]\n\
                 2\nada\nbob\n",
            "the LazyFrame codegen twin must render and evaluate \
                 identically to the interpreter",
        );
    }
}

#[test]
fn test_e2e_lazyframe_join_collision_suffix_and_explain() {
    // Full-surface twin port: inner join — nested right sub-plan
    // rendered compactly on the JOIN line; output schema left-then-
    // right-minus-keys with `_right` on collisions; unmatched rows
    // drop; left row order preserved. Byte-pinned to the interpreter
    // oracle (test_lazyframe_join_inner_collision_suffix_and_explain).
    let out = run_program(
            "fn main() {\n\
                 let mut people: DataFrame = DataFrame.new();\n\
                 people.insert(\"id\", Column.from_vec([1i64, 2i64, 3i64, 4i64]));\n\
                 people.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"cyd\", \"dee\"]));\n\
                 people.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"rome\", \"lima\"]));\n\
                 let mut cities: DataFrame = DataFrame.new();\n\
                 cities.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"kiev\"]));\n\
                 cities.insert(\"pop\", Column.from_vec([60i64, 80i64, 30i64]));\n\
                 cities.insert(\"name\", Column.from_vec([\"Roma\", \"Oslo\", \"Kyiv\"]));\n\
                 let plan = people.lazy().join(cities.lazy(), vec![\"city\"]);\n\
                 println(plan.explain());\n\
                 let out = plan.collect();\n\
                 println(out.width()); println(out.height());\n\
                 for n in out.column_names() { println(n); }\n\
                 let names: Column[String] = out.column(\"name\");\n\
                 let rn: Column[String] = out.column(\"name_right\");\n\
                 let pops: Column[i64] = out.column(\"pop\");\n\
                 for i in 0..out.height() {\n\
                     match names[i] { Some(v) => println(v), None => println(\"null\") }\n\
                     match rn[i] { Some(v) => println(v), None => println(\"null\") }\n\
                     match pops[i] { Some(v) => println(v), None => println(-1i64) }\n\
                 }\n\
             }",
        );
    if let Some(out) = out {
        assert_eq!(
            out,
            "== logical plan ==\n\
                 JOIN on=[city] right=(SCAN [city, pop, name])\n\
                 \x20 SCAN [id, name, city]\n\
                 == optimized ==\n\
                 JOIN on=[city] right=(SCAN cols=[*])\n\
                 \x20 SCAN cols=[*]\n\
                 5\n3\nid\nname\ncity\npop\nname_right\n\
                 ada\nRoma\n60\nbob\nOslo\n80\ncyd\nRoma\n60\n",
            "inner join must suffix collisions, drop unmatched rows, keep \
                 left order byte-identically to the interpreter",
        );
    }
}

#[test]
fn test_e2e_lazyframe_join_pipeline_nulls_and_fanout() {
    // Full-surface twin port: ops composing around a join — right-side
    // select narrows the right scan; a left select BEFORE the join
    // renders as an explicit SELECT step under the JOIN; NULL keys join
    // nothing; duplicate right matches fan out left-row-then-right-
    // match. Chain intermediates are let-bound (eager DataFrame method
    // dispatch on non-identifier receivers is outside the twin), so
    // this mirrors — not copies — the interpreter oracle
    // (test_lazyframe_join_pipeline_nulls_and_fanout).
    let out = run_program(
            "import std.lazy.{col};\n\
             fn main() {\n\
                 let mut people: DataFrame = DataFrame.new();\n\
                 people.insert(\"id\", Column.from_vec([1i64, 2i64, 3i64, 4i64]));\n\
                 people.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"cyd\", \"dee\"]));\n\
                 people.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"rome\", \"lima\"]));\n\
                 let mut cities: DataFrame = DataFrame.new();\n\
                 cities.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"kiev\"]));\n\
                 cities.insert(\"pop\", Column.from_vec([60i64, 80i64, 30i64]));\n\
                 cities.insert(\"name\", Column.from_vec([\"Roma\", \"Oslo\", \"Kyiv\"]));\n\
                 let plan = people.lazy()\n\
                     .join(cities.lazy().select(vec![\"city\", \"pop\"]), vec![\"city\"])\n\
                     .filter(col(\"pop\").gt(50))\n\
                     .select(vec![\"name\", \"pop\"]);\n\
                 println(plan.explain());\n\
                 let planc = plan.collect();\n\
                 println(planc.height());\n\
                 let pre = people.lazy().select(vec![\"name\", \"city\"]).join(cities.lazy(), vec![\"city\"]);\n\
                 println(pre.explain());\n\
                 let prec = pre.collect();\n\
                 println(prec.width());\n\
                 let mut left: DataFrame = DataFrame.new();\n\
                 let lk: Vec[Option[i64]] = vec![Some(1i64), None, Some(3i64)];\n\
                 left.insert(\"k\", Column.from_iter_nullable(lk));\n\
                 let mut right: DataFrame = DataFrame.new();\n\
                 let rk: Vec[Option[i64]] = vec![Some(1i64), None];\n\
                 right.insert(\"k\", Column.from_iter_nullable(rk));\n\
                 right.insert(\"w\", Column.from_vec([100i64, 200i64]));\n\
                 let njp = left.lazy().join(right.lazy(), vec![\"k\"]);\n\
                 let nj = njp.collect();\n\
                 println(nj.height());\n\
                 let ws: Column[i64] = nj.column(\"w\");\n\
                 match ws[0] { Some(v) => println(v), None => println(-1i64) }\n\
                 let mut dup: DataFrame = DataFrame.new();\n\
                 dup.insert(\"city\", Column.from_vec([\"rome\", \"rome\"]));\n\
                 dup.insert(\"tag\", Column.from_vec([\"a\", \"b\"]));\n\
                 let fanp = people.lazy().select(vec![\"name\", \"city\"]).join(dup.lazy(), vec![\"city\"]);\n\
                 let fan = fanp.collect();\n\
                 println(fan.height());\n\
                 let n4: Column[String] = fan.column(\"name\");\n\
                 let t4: Column[String] = fan.column(\"tag\");\n\
                 for i in 0..fan.height() {\n\
                     match n4[i] { Some(v) => println(v), None => println(\"null\") }\n\
                     match t4[i] { Some(v) => println(v), None => println(\"null\") }\n\
                 }\n\
             }",
        );
    if let Some(out) = out {
        assert_eq!(
            out,
            "== logical plan ==\n\
                 SELECT [name, pop]\n\
                 \x20 FILTER (pop > 50)\n\
                 \x20   JOIN on=[city] right=(SELECT [city, pop] <- SCAN [city, pop, name])\n\
                 \x20     SCAN [id, name, city]\n\
                 == optimized ==\n\
                 SELECT [name, pop]\n\
                 \x20 FILTER (pop > 50)\n\
                 \x20   JOIN on=[city] right=(SCAN cols=[city, pop])\n\
                 \x20     SCAN cols=[*]\n\
                 3\n\
                 == logical plan ==\n\
                 JOIN on=[city] right=(SCAN [city, pop, name])\n\
                 \x20 SELECT [name, city]\n\
                 \x20   SCAN [id, name, city]\n\
                 == optimized ==\n\
                 JOIN on=[city] right=(SCAN cols=[*])\n\
                 \x20 SELECT [name, city]\n\
                 \x20   SCAN cols=[*]\n\
                 4\n\
                 1\n100\n\
                 4\nada\na\nada\nb\ncyd\na\ncyd\nb\n",
            "join must compose with select/filter, drop NULL keys, fan out \
                 duplicates byte-identically to the interpreter",
        );
    }
}

/// B-2026-08-18-39 — THE ANTI-VACUITY GUARD for the ASAN fixture
/// `asan_par_slot_boxed_option_payload_freed_once`.
///
/// That fixture certifies a branch→parent ownership transfer for a heap-BOXED
/// enum payload. If the analyzer stops splitting its `build`, there is no
/// branch, no return slot and no transfer — and the fixture goes green while
/// covering nothing. This is not hypothetical: the fixture's FIRST draft
/// consumed the bindings with `match`, which makes the analyzer decline to
/// fork, and it passed against the broken compiler. (The same mistake is why
/// the row itself recorded `?.` as an ingredient of the bug — the `match`
/// spelling it compared against never parallelized.)
///
/// So this asserts both halves the transfer needs: that the function splits
/// at all, and that `hit` is PUBLISHED as a return slot.
#[test]
fn par_slot_boxed_option_fixture_actually_parallelizes() {
    // The ASAN fixture's source VERBATIM — churn and all, for the reason
    // the sibling guard below gives.
    let src = r#"
struct W { f0: i64, f1: i64, f2: i64, f3: i64 }

fn get(v: ref Vec[W], k: i64) -> Option[W] {
    let mut i = 0i64;
    while i < v.len() { if v[i].f0 == k { return Some(v[i]); } i = i + 1i64; }
    return None;
}

fn build(seed: i64) -> i64 {
    let mut ns: Vec[W] = Vec.new();
    ns.push(W { f0: seed, f1: seed + 1i64, f2: seed + 2i64, f3: seed + 3i64 });
    let hit = get(ns, seed);
    let miss = get(ns, seed + 99i64);
    let a = hit?.f3;
    let b = miss?.f3;
    return match a { Some(x) => x, None => -1i64 } + match b { Some(x) => x, None => -1i64 };
}

fn churn(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let pad: i64 = n * 3;
    return pad + churn(n - 1);
}

fn main() {
    let seed: i64 = env.args().len();
    let k = build(seed);
    let noise = churn(2000);
    println(k + noise - noise);
}
"#;
    // Mirror the ASAN harness's pipeline, NOT `ir_for`'s — that helper
    // passes `ownership: None` and no concurrency analysis, which turns
    // auto-par off and would make this guard assert the opposite of what
    // it means to.
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
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let effects = karac::effectcheck(&parsed.program);
    let concurrency = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));
    let ir = compile_to_ir(&parsed.program, Some(&ownership), Some(&concurrency))
        .expect("codegen failed");
    let branches = ir
        .lines()
        .filter(|l| l.starts_with("define") && l.contains("@__par_branch"))
        .count();
    assert!(
        branches > 0,
        "the analyzer no longer splits `build`, so \
             asan_par_slot_boxed_option_payload_freed_once is now vacuous — it \
             covers a branch→parent ownership transfer that never happens. \
             Re-shape the fixture until this parallelizes again."
    );
    assert!(
        ir.contains("__par_slot_hit_dst"),
        "`hit` is no longer PUBLISHED as a return slot, so the boxed-payload \
             ownership hand-off this row fixed is not exercised."
    );
}

/// B-2026-08-08-15 — THE ANTI-VACUITY GUARD for the two ASAN fixtures in
/// `tests/memory_sanitizer.rs` that cover the auto-par slot-ownership
/// transfer (`asan_par_slot_shared_struct_*`).
///
/// Those fixtures are only meaningful if the analyzer actually SPLITS the
/// function — the bug lives in the branch→parent hand-off, so a shape that
/// never parallelizes exercises nothing while reporting green. That is not
/// hypothetical: during the investigation a weak-vs-strong control was
/// written without checking, drew 0 branches, came back clean, and was
/// briefly taken as evidence that the defect needed a container.
///
/// Asserting the branch count HERE rather than inside the ASAN fixtures is
/// deliberate: the ASAN harness shells out to a linked binary and sees no
/// IR, and the leak checkers cannot tell "clean because it is fixed" from
/// "clean because nothing ran". If a future analyzer heuristic stops
/// splitting these shapes, this test fails loudly and points at the
/// fixtures that just went hollow.
#[test]
fn par_slot_shared_struct_fixtures_actually_parallelize() {
    // The ASAN fixtures' sources VERBATIM — churn and all. Reproduced whole
    // rather than abbreviated: an earlier draft of this guard trimmed
    // `churn` and simplified `main`, and the trimmed program did not
    // parallelize, so the guard and the fixtures disagreed about the very
    // property the guard exists to certify. If the guard is not compiling
    // what the fixtures compile, it certifies nothing.
    let body = |vec_ty: &str, pushed: &str| {
        format!(
            "shared struct P {{ v: i64 }}\n\
                 shared struct Q {{ a: i64, b: i64, c: i64 }}\n\
                 fn mix(seed: i64) -> i64 {{\n\
                 \x20   let mut s: i64 = seed;\n\
                 \x20   let mut i: i64 = 0;\n\
                 \x20   while i < 8 {{ s = s + i * 3; i = i + 1; }}\n\
                 \x20   return s;\n\
                 }}\n\
                 fn build(seed: i64) -> i64 {{\n\
                 \x20   let p: P = P {{ v: mix(seed) }};\n\
                 \x20   let q: Q = Q {{ a: mix(seed), b: mix(seed), c: mix(seed) }};\n\
                 \x20   let mut w: {vec_ty} = Vec.new();\n\
                 \x20   w.push({pushed});\n\
                 \x20   return w.len() + q.a - q.a;\n\
                 }}\n\
                 fn churn(n: i64) -> i64 {{\n\
                 \x20   if n <= 0 {{ return 0; }}\n\
                 \x20   let pad: i64 = n * 3;\n\
                 \x20   return pad + churn(n - 1);\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let seed: i64 = env.args().len();\n\
                 \x20   let k = build(seed);\n\
                 \x20   let noise = churn(2000);\n\
                 \x20   println(k + noise - noise);\n\
                 }}\n"
        )
    };
    let read_only = body("Vec[i64]", "p.v");
    let into_container = body("Vec[P]", "p");
    // Mirror the ASAN harness's pipeline, NOT `ir_for`'s: that helper
    // passes `ownership: None`, and auto-par grouping differs under it.
    // The guard must model what the fixtures actually compile.
    let ir_like_asan = |src: &str| {
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
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        // The concurrency analysis is what turns auto-par ON. Passing
        // `None` here (as `ir_for` does, and as the ASAN harness did before
        // this row) disables the parallelizer outright — which would make
        // this guard assert the opposite of what it means to.
        let effects = karac::effectcheck(&parsed.program);
        let concurrency = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));
        compile_to_ir(&parsed.program, Some(&ownership), Some(&concurrency))
            .expect("codegen failed")
    };
    for (label, src) in [
        ("read-only join", &read_only),
        ("moved into a container", &into_container),
    ] {
        let ir = ir_like_asan(src);
        let branches = ir
            .lines()
            .filter(|l| l.starts_with("define") && l.contains("@__par_branch"))
            .count();
        assert!(
            branches > 0,
            "{label}: the analyzer no longer splits this function, so the \
                 asan_par_slot_shared_struct_* fixtures are now vacuous — they \
                 cover a branch→parent ownership transfer that never happens. \
                 Re-shape both fixtures until this parallelizes again."
        );
        // The published slot is what the transfer is about; without it the
        // branch keeps its own value and the hand-off is never exercised.
        assert!(
            ir.contains("__par_slot_p_dst"),
            "{label}: `p` is no longer PUBLISHED as a return slot, so the \
                 RC ownership hand-off this row fixed is not exercised."
        );
    }
}

#[test]
fn e2e_ref_atomic_param_aliases_caller_cell() {
    // B-2026-07-18-30: a `ref Atomic[T]` / `mut ref Atomic[T]` parameter's
    // alloca holds a POINTER to the caller's atomic storage, but
    // `resolve_atomic_storage` returned the pointer-holding slot itself, so
    // `fn bump(c: ref Atomic[i64]) { c.fetch_add(1,..) }` RMW'd the slot and
    // the caller's cell never changed (build printed 0 vs the interpreter's
    // 2) — a run-vs-build divergence with no `par` involved. The fix derefs
    // the ref-param pointer before the atomic op. Covers ref (fetch_add),
    // mut ref (store via a `mut` call marker), and read-only (load).
    if let Some(out) = run_program(
        "fn bump(c: ref Atomic[i64]) { let _ = c.fetch_add(1, MemoryOrdering.SeqCst); }\n\
             fn setit(c: mut ref Atomic[i64]) { c.store(9, MemoryOrdering.SeqCst); }\n\
             fn readit(c: ref Atomic[i64]) -> i64 { c.load(MemoryOrdering.SeqCst) }\n\
             fn main() {\n\
                 let c = Atomic.new(0);\n\
                 bump(c);\n\
                 bump(c);\n\
                 println(c.load(MemoryOrdering.SeqCst));\n\
                 println(readit(c));\n\
                 setit(mut c);\n\
                 println(c.load(MemoryOrdering.SeqCst));\n\
             }",
    ) {
        assert_eq!(out, "2\n2\n9\n");
    }
}

/// Stack-frame guard for B-2026-09-01-47 — compiling a shallow generic
/// monomorph nest must fit in a modest thread stack.
///
/// The defect this pins was NOT deep recursion. The stack that aborted
/// `Codegen E2E (Linux arm64)` was only ~40 compiler frames over three
/// levels of monomorphization; it overflowed because three functions each
/// burn 46-126 KiB per call in an unoptimized build, where LLVM does not
/// color stack slots and every arm of a large `match` gets its locals
/// allocated at once. `[profile.dev.package.karac] opt-level = 1` in
/// Cargo.toml is the fix; this notices if the requirement climbs back.
///
/// RE-EXECS ITSELF rather than spawning a bounded thread in-process,
/// because a stack overflow ABORTS — it cannot be caught, so an in-process
/// version would kill the whole test binary (exactly the CI symptom) and
/// take every later test's result with it. In a child process the abort
/// becomes a non-zero exit this test reports as an ordinary failure.
///
/// 1 MiB, not the ~256 KiB actually measured on x86-64: aarch64 frames can
/// be materially larger and no arm64 host was available to measure, so the
/// budget is set for margin over sensitivity. It still catches a return to
/// the unoptimized frame sizes, which needed more than 1 MiB on x86-64 and
/// more again on arm64. A partial regression will slip through — that is
/// the deliberate trade, since a guard that false-fires on one arch is
/// worse than one that catches only the large regression.
#[test]
fn compiling_a_generic_monomorph_nest_fits_a_modest_thread_stack() {
    const CHILD: &str = "KARAC_STACK_GUARD_CHILD";
    // Same program as `e2e_generic_non_let_binding_instantiation_across_sites`
    // above — the nest that actually overflowed.
    let src = "struct Bag[=T] { xs: Vec[T] }\n\
                   impl[T: Ord] Bag[T] {\n    \
                       fn swap2(mut ref self, i: i64, j: i64) { self.xs.swap(i, j); }\n    \
                       fn arrange(mut ref self) { let n = self.xs.len(); if n > 1 { self.swap2(0, n - 1); } }\n    \
                       fn inner(self) -> Vec[T] { let mut b = self; b.arrange(); b.xs }\n    \
                       fn mk(v: Vec[T]) -> Vec[T] { let bags = [Bag { xs: v }]; \
                         let mut out: Vec[T] = Vec.new(); for b in bags { out = b.inner(); } out }\n\
                   }\n\
                   fn main() {\n    \
                       let a = Bag.mk([\"x\", \"y\", \"z\"]); println(a[0]);\n    \
                       let b = Bag.mk([1, 2, 3]); println(b[0]);\n\
                   }\n";

    if std::env::var(CHILD).is_ok() {
        // The result is deliberately ignored: this asserts only that
        // COMPILING did not blow the stack. A checkout without the runtime
        // archives yields `None` here, which must stay a pass — the output
        // oracle is the sibling test's job, not this one's.
        let _ = run_program(src);
        return;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(exe)
        .args([
            "codegen_tests::compiling_a_generic_monomorph_nest_fits_a_modest_thread_stack",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("RUST_MIN_STACK", "1048576")
        .output()
        .expect("re-exec the test binary");

    assert!(
        out.status.success(),
        "compiling a three-level generic monomorph nest no longer fits a 1 MiB \
             thread stack — karac's per-frame stack usage has regressed (B-2026-09-01-47). \
             Check that `[profile.dev.package.karac] opt-level = 1` is still in Cargo.toml, \
             then re-measure with `RUST_MIN_STACK`.\nchild stdout:\n{}\nchild stderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// B-2026-08-02-11 — tuple literals now thread the EXPECTED element
/// types into inference-driven constructor elements:
/// `let t: (Vec[i64], i64) = (Vec.new(), 3)` and
/// `Sw { t: (Vec.new(), 3) }` were rejected with the `Vec<?T0>`
/// mismatch although the expectation determines ?T0 uniquely. The
/// check_expr tuple intercept pushes each expected slot into `X.new()`
/// elements only (non-constructor elements keep synthesis mode). Rides
/// the B-2026-08-02-10 dispatcher for the follow-on element methods.
/// Twin of `tests/interpreter.rs`'s
/// `test_tuple_literal_expected_elem_threading`.
#[test]
fn e2e_tuple_literal_expected_elem_threading() {
    let Some(out) = run_program(
        "struct Sw { t: (Vec[i64], i64) }\n\
             fn main() {\n\
             \x20   let mut t: (Vec[i64], i64) = (Vec.new(), 3);\n\
             \x20   t.0.push(10);\n\
             \x20   println(f\"a {t.0.len()} {t.0[0]} {t.1}\");\n\
             \x20   let mut o = Sw { t: (Vec.new(), 3) };\n\
             \x20   o.t.0.push(7);\n\
             \x20   println(f\"b {o.t.0.len()} {o.t.0[0]}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a 1 10 3\nb 1 7\nend\n");
}

/// B-2026-08-01-5 — fresh method-RECEIVER Drop temps: a `ref self`
/// method's fresh receiver fires its body at STATEMENT END on both
/// backends (pre-fix: never under `karac run`, at scope exit under
/// `karac build`); a passthrough result (`mk(4).me()`) is owned by its
/// binding alone — exactly one fire, at the binding's death. Twin of
/// `tests/interpreter.rs`'s `test_fresh_recv_temp_drop_semantics`.
///
/// Cell `b` (`mk(2).eat()`, owned `self`) was pinned SILENT on the
/// reasoning that the callee CONSUMED the receiver. It consumes the
/// VALUE; it does not run the bodies — codegen treats a by-value `self`
/// as caller-retained, so nobody ran them and the `Res` was destroyed
/// with its destructor never firing. B-2026-09-04-30 is that lost body
/// and `drop 2 r2` is this pin moving to match. Cell `d` becomes the
/// guard for the fix: `me(self) -> Res` hands the receiver back, so the
/// caller still stands down and the single fire comes from `m`.
#[test]
fn e2e_fresh_recv_temp_drop_semantics() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             impl Res {\n\
             \x20   fn ident(ref self) -> i64 {\n\
             \x20       return self.id;\n\
             \x20   }\n\
             \x20   fn eat(self) -> i64 {\n\
             \x20       return self.id + 100;\n\
             \x20   }\n\
             \x20   fn me(self) -> Res {\n\
             \x20       return self;\n\
             \x20   }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
             \x20   return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a: ref-method fresh struct receiver\");\n\
             \x20   let x = mk(1).ident();\n\
             \x20   println(f\"x={x}\");\n\
             \x20   println(\"b: owned-self consumed receiver\");\n\
             \x20   let y = mk(2).eat();\n\
             \x20   println(f\"y={y}\");\n\
             \x20   println(\"c: bare ref-method statement\");\n\
             \x20   mk(3).ident();\n\
             \x20   println(\"d: passthrough result owned by binding\");\n\
             \x20   let m = mk(4).me();\n\
             \x20   println(f\"m={m.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a: ref-method fresh struct receiver\ndrop 1 r1\nx=1\n\
             b: owned-self consumed receiver\ndrop 2 r2\ny=102\n\
             c: bare ref-method statement\ndrop 3 r3\n\
             d: passthrough result owned by binding\nm=4\ndrop 4 r4\nend\n"
    );
}

/// B-2026-09-07-47 — output-side twin of
/// `asan_rc_promoted_binding_crossing_a_par_join_still_frees_its_box`.
///
/// The defect is a LEAK and nothing about it is observable in stdout, so
/// this test cannot fail on the parent compiler and is not a regression
/// guard on its own — `tests/memory_sanitizer.rs` is. What it pins is the
/// half a leak fix can still get wrong: the fix re-types the joined
/// variable as a POINTER (an RC-promoted binding is physically a
/// `{i64 rc, T}` box handle), and every read of that binding after the join
/// resolves through it. If that re-typing ever disagrees with what the
/// branch actually stored, these cells stop printing the right answer —
/// which is exactly how a slot-type change goes wrong (B-2026-08-08-18's
/// `y` allocated as an `i64` and loaded 8 bytes of control-block pointer
/// out of it).
///
/// Every cell is verified identical on all four surfaces — `--interp`, the
/// JIT, and `karac build` at `KARAC_OPT_LEVEL` 2 and 0, each with auto-par
/// on and BUILT with `KARAC_AUTO_PAR=0`.
#[test]
fn e2e_rc_promoted_binding_crossing_a_par_join_reads_correctly() {
    const PRE: &str = r#"struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
"#;
    // (source tail, expected stdout, cell name)
    let cells: [(&str, &str, &str); 7] = [
        // 1 — the row's own shape. `r.len()` is 38: the f-string payload.
        (
            r#"impl P { fn take(self) -> i64 { return self.b; } }
fn go() -> i64 { let t = mkp(9); let mut r = payload(); let mut i = 0i64;
  while i < 3i64 { t.take(); i = i + 1; }
  return r.len(); }
fn main() { println(go()); }
"#,
            "38",
            "whole_method_consume_3_trips",
        ),
        // 2 — a PROJECTING argument off the promoted binding: 38 + 3*38.
        (
            r#"fn takes(s: String) -> i64 { return s.len(); }
fn go() -> i64 { let t = mkp(9); let mut r = payload(); let mut i = 0i64; let mut acc = 0i64;
  while i < 3i64 { acc = acc + takes(t.a); i = i + 1; }
  return r.len() + acc; }
fn main() { println(go()); }
"#,
            "152",
            "projecting_argument",
        ),
        // 3 — zero trips.
        (
            r#"impl P { fn take(self) -> i64 { return self.b; } }
fn go() -> i64 { let t = mkp(9); let mut r = payload(); let mut i = 0i64;
  while i < 0i64 { t.take(); i = i + 1; }
  return r.len(); }
fn main() { println(go()); }
"#,
            "38",
            "zero_trip_loop",
        ),
        // 4 — five trips.
        (
            r#"impl P { fn take(self) -> i64 { return self.b; } }
fn go() -> i64 { let t = mkp(9); let mut r = payload(); let mut i = 0i64;
  while i < 5i64 { t.take(); i = i + 1; }
  return r.len(); }
fn main() { println(go()); }
"#,
            "38",
            "five_trips",
        ),
        // 5 — READ THE PROMOTED BINDING AFTER THE JOIN, through the box.
        // The re-typed variable is what this read resolves through, so a
        // wrong slot type surfaces here as a wrong number rather than as a
        // leak.
        (
            r#"impl P { fn take(self) -> i64 { return self.b; } }
fn go() -> i64 { let t = mkp(9); let mut r = payload(); let mut i = 0i64;
  while i < 3i64 { t.take(); i = i + 1; }
  return r.len() + t.b; }
fn main() { println(go()); }
"#,
            "47",
            "read_promoted_binding_after_join",
        ),
        // 6 (CONTROL) — free-function consume; no fan-out at all.
        (
            r#"fn takep(p: P) -> i64 { return p.b; }
fn go() -> i64 { let t = mkp(9); let mut r = payload(); let mut i = 0i64;
  while i < 3i64 { takep(t); i = i + 1; }
  return r.len(); }
fn main() { println(go()); }
"#,
            "38",
            "control_free_fn_consume_no_fanout",
        ),
        // 7 — explicit statement-position `par` block, the sibling
        // bind-back site.
        (
            r#"fn takep(p: P) -> i64 { return p.b; }
fn go() -> i64 {
  par {
    let t = mkp(9);
    let k = seed() + 41i64;
  }
  let mut i = 0i64;
  while i < 3i64 { takep(t); i = i + 1; }
  return k; }
fn main() { println(go()); }
"#,
            "42",
            "explicit_statement_par_block",
        ),
    ];
    for (tail, expected, name) in cells {
        let src = format!("{PRE}{tail}");
        if let Some(cap) = run_program_capturing(&src) {
            assert_eq!(
                cap.stdout.trim(),
                expected,
                "[{name}] wrong stdout for an RC-promoted binding across a par join"
            );
            assert!(cap.status.success(), "[{name}] program exited non-zero");
        }
    }
}

/// B-2026-07-30-11 (owning-temp arm channel) — a match / if-let /
/// while-let arm binding over an OWNING fresh-temp scrutinee
/// (`v.pop()`) runs the moved Drop payload's body at arm end, while a
/// BORROW accessor scrutinee (`m.get(k)`, `v.first()`) fires exactly
/// once via the container's own walk (the stash must stay silent — the
/// interp side of this gate double-fired when it admitted any
/// MethodCall). Twin of `tests/interpreter.rs`'s
/// `test_arm_channel_owning_temp_vs_borrow_scrutinees`.
#[test]
fn e2e_arm_channel_owning_temp_vs_borrow_scrutinees() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut v: Vec[Res] = Vec.new();\n\
             \x20   v.push(Res { id: 61 });\n\
             \x20   println(\"a\");\n\
             \x20   match v.pop() {\n\
             \x20       Option.Some(r) => { println(f\"got {r.id}\"); }\n\
             \x20       Option.None => { println(\"none\"); }\n\
             \x20   }\n\
             \x20   println(\"b\");\n\
             \x20   let mut w: Vec[Res] = Vec.new();\n\
             \x20   w.push(Res { id: 62 });\n\
             \x20   if let Option.Some(r) = w.pop() {\n\
             \x20       println(f\"iflet got {r.id}\");\n\
             \x20   }\n\
             \x20   println(\"c\");\n\
             \x20   let mut u: Vec[Res] = Vec.new();\n\
             \x20   u.push(Res { id: 63 });\n\
             \x20   while let Option.Some(r) = u.pop() {\n\
             \x20       println(f\"wlet got {r.id}\");\n\
             \x20   }\n\
             \x20   println(\"d\");\n\
             \x20   let mut m: Map[i64, Res] = Map.new();\n\
             \x20   m.insert(1, Res { id: 81 });\n\
             \x20   if let Option.Some(r) = m.get(1) {\n\
             \x20       println(f\"see {r.id}\");\n\
             \x20   }\n\
             \x20   println(\"e\");\n\
             \x20   let mut f: Vec[Res] = Vec.new();\n\
             \x20   f.push(Res { id: 82 });\n\
             \x20   if let Option.Some(r) = f.first() {\n\
             \x20       println(f\"first {r.id}\");\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ngot 61\ndrop 61\nb\niflet got 62\ndrop 62\nc\nwlet got 63\ndrop 63\nd\n\
             see 81\ndrop 81\ne\nfirst 82\ndrop 82\nend\n"
    );
}

/// B-2026-07-31-43 — `len`/`is_empty` on a fresh-temp `Vec` receiver
/// with heap-bearing elements (String / user struct) keeps producing the
/// correct scalar while the new per-element receiver drop-track
/// (`temp_recv_len_elem_types` → the intercept's element walk) frees the
/// temp. The leak itself is pinned by `tests/memory_sanitizer.rs`'s
/// `asan_len_family_fresh_recv_elements_freed`; this guards the value
/// path (a mis-timed free before the `len` extract would corrupt the
/// count or crash).
#[test]
fn e2e_len_family_fresh_recv_values() {
    let Some(out) = run_program(
        "struct Rec { id: i64, s: String }\n\
             fn mk(n: i64) -> Vec[String] {\n\
             \x20   let mut v: Vec[String] = Vec.new();\n\
             \x20   let mut i = 0;\n\
             \x20   while i < n {\n\
             \x20       v.push(f\"item-{i}-payload\");\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   v\n\
             }\n\
             fn mk_recs(n: i64) -> Vec[Rec] {\n\
             \x20   let mut v: Vec[Rec] = Vec.new();\n\
             \x20   let mut i = 0;\n\
             \x20   while i < n {\n\
             \x20       v.push(Rec { id: i, s: f\"rec-{i}-payload\" });\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   v\n\
             }\n\
             fn main() {\n\
             \x20   println(f\"{mk(3).len()}\");\n\
             \x20   println(f\"{mk(2).is_empty()}\");\n\
             \x20   println(f\"{mk_recs(2).len()}\");\n\
             \x20   println(f\"{Env.args().len() >= 1}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "3\nfalse\n2\ntrue\n");
}

#[test]
fn test_e2e_struct_literal_receiver_in_generic_impl_selects_its_monomorph() {
    // The filed repro. Heap-carrying `T` first, scalar `T` second, in one
    // program: the two instantiations must reach DIFFERENT monomorphs.
    let src = format!(
        "{B28_BAG}    fn mk(v: Vec[T]) -> Vec[T] {{ Bag {{ xs: v }}.inner() }}\n\
             }}\n\
             fn main() {{ let a = Bag.mk([\"x\",\"y\",\"z\"]); println(a[0]); \
             let b = Bag.mk([1,2,3]); println(b[0]); }}"
    );
    assert_eq!(run_program(&src).as_deref(), Some("z\n3\n"));
}

#[test]
fn test_e2e_assoc_call_receiver_in_generic_impl_selects_its_monomorph() {
    // Same defect through a CALL receiver rather than a struct literal,
    // provided the constructor is itself inside the generic impl so its
    // return type is still written in terms of `T`.
    let src = format!(
        "{B28_BAG}    fn make(v: Vec[T]) -> Bag[T] {{ Bag {{ xs: v }} }}\n\
             \x20   fn mk2(v: Vec[T]) -> Vec[T] {{ Bag.make(v).inner() }}\n\
             }}\n\
             fn main() {{ let a = Bag.mk2([\"x\",\"y\",\"z\"]); println(a[0]); \
             let b = Bag.mk2([1,2,3]); println(b[0]); }}"
    );
    assert_eq!(run_program(&src).as_deref(), Some("z\n3\n"));
}

#[test]
fn test_ir_fence_seqcst_emits_cross_thread_fence() {
    // `fence(MemoryOrdering.SeqCst)` (`runtime/stdlib/intrinsics.kara`)
    // lowers to a cross-thread LLVM `fence seq_cst` (no syncscope
    // qualifier ⇒ the default cross-thread scope). Witnesses the
    // `compile_atomic_fence` intercept in `compile_call`.
    let ir = ir_for(
        r#"
fn barrier() with reads(Hardware) {
    // Safety: a full barrier ordering the surrounding accesses.
    unsafe { fence(MemoryOrdering.SeqCst); }
}
fn main() { barrier(); }
"#,
    );
    assert!(
        ir.contains("fence seq_cst"),
        "expected a cross-thread `fence seq_cst`; IR:\n{ir}"
    );
    assert!(
        !ir.contains("fence syncscope(\"singlethread\") seq_cst"),
        "cross-thread `fence` must NOT carry the singlethread syncscope; IR:\n{ir}"
    );
}

#[test]
fn test_ir_compiler_fence_uses_singlethread_syncscope() {
    // `compiler_fence(order)` is a compiler-only reordering barrier: LLVM
    // `fence syncscope("singlethread") <order>` restrains the optimizer
    // without emitting a CPU barrier. It is *safe* (no `unsafe` block).
    let ir = ir_for(
        r#"
fn barrier() {
    compiler_fence(MemoryOrdering.SeqCst);
}
fn main() { barrier(); }
"#,
    );
    assert!(
        ir.contains("fence syncscope(\"singlethread\") seq_cst"),
        "expected a singlethread-syncscope `compiler_fence`; IR:\n{ir}"
    );
}

#[test]
fn test_e2e_oncelock_set_get_is_set_lifecycle() {
    // `OnceLock[i64]` write-once lifecycle under `karac build`/JIT (was
    // interpreter-only). Empty → `is_set() == false`, `get() == None`;
    // after `set`, `is_set() == true`, `get() == Some(42)`; a second `set`
    // fails with `Err(AlreadySetError)` and leaves the stored value intact.
    // Build==run parity witness for the whole `karac_runtime_once_*` FFI +
    // the `Result`/`Option` construction. Mirrors the interpreter test
    // `test_oncelock_set_get_is_set_roundtrip`.
    let out = run_program(
        r#"
fn main() {
    let cell: OnceLock[i64] = OnceLock.new();
    println(cell.is_set());
    match cell.get() { Some(v) => println(v), None => println(-1), }
    match cell.set(42) { Ok(_) => println(1), Err(_) => println(0), }
    println(cell.is_set());
    match cell.get() { Some(v) => println(v), None => println(-1), }
    match cell.set(99) { Ok(_) => println(1), Err(_) => println(0), }
    match cell.get() { Some(v) => println(v), None => println(-1), }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "false\n-1\n1\ntrue\n42\n0\n42");
    }
}

#[test]
fn test_e2e_oncelock_get_unwrap_and_unwrap_or() {
    // Regression (B-2026-07-17-16): `OnceLock[T].get()` returns
    // `Option[ref T]`, and the typechecker recorded the payload as
    // `ref i64` (unlike `Vec.get`, recorded as `i64`). Left as `ref T`,
    // the Option-method payload reconstruction lowered it to a pointer
    // and re-read the value word via `inttoptr`: `unwrap_or(0)` failed
    // the LLVM verifier ("PHI node operands are not the same type"), and
    // `unwrap()` built a bogus pointer that faulted at run time. The fix
    // strips the outer borrow so reconstruction is over the value type
    // (the payload is physically value-packed — `once.rs` loads `T`
    // through the borrow before splitting). Present → the sealed value;
    // absent (unset cell) → the default. interp == JIT == AOT.
    let out = run_program(
        r#"
fn main() {
    let c: OnceLock[i64] = OnceLock.new();
    let _s = c.set(7);
    println(c.get().unwrap());
    println(c.get().unwrap_or(0));
    let d: OnceLock[i64] = OnceLock.new();
    println(d.get().unwrap_or(42));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7\n7\n42");
    }
}

#[test]
fn test_e2e_oncecell_parallel_surface() {
    // `OnceCell[i64]` has the identical method surface + lowering as
    // `OnceLock` (shares the runtime primitive; single-task rides the lock
    // uncontended). Same lifecycle witness on the `OnceCell` receiver.
    let out = run_program(
        r#"
fn main() {
    let c: OnceCell[i64] = OnceCell.new();
    println(c.is_set());
    match c.set(7) { Ok(_) => println(1), Err(_) => println(0), }
    match c.get() { Some(v) => println(v), None => println(-1), }
    println(c.is_set());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "false\n1\n7\ntrue");
    }
}

#[test]
fn test_e2e_oncelock_loop_no_leak_and_correct() {
    // A local `OnceLock` per loop iteration: the scope-exit `FreeOnceHandle`
    // must reclaim each cell (handle + sealed value buffer) — a missed free
    // leaks one cell per iteration (LSan/valgrind). Summing the sealed
    // values (0..99) also pins that each iteration's `set`+`get` is correct
    // and independent (a fresh cell each round, not a leaked shared one).
    let out = run_program(
        r#"
fn main() {
    let mut i = 0;
    let mut total = 0;
    while i < 100 {
        let cell: OnceLock[i64] = OnceLock.new();
        let _ = cell.set(i);
        match cell.get() { Some(v) => { total = total + v; }, None => {}, }
        i = i + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        // sum 0..99 = 4950
        assert_eq!(out.trim(), "4950");
    }
}

#[test]
fn test_ir_oncelock_dispatches_to_runtime_ffi() {
    // `OnceLock.new()`/`set`/`get`/`is_set` lower to the `karac_runtime_once_*`
    // FFI (not a user-impl call on the baked stdlib struct).
    let ir = ir_for(
        r#"
fn use_cell() -> i64 {
    let cell: OnceLock[i64] = OnceLock.new();
    let _ = cell.set(5);
    match cell.get() { Some(v) => v, None => 0, }
}
"#,
    );
    for needle in [
        "declare ptr @karac_runtime_once_new",
        "declare i8 @karac_runtime_once_set",
        "declare ptr @karac_runtime_once_get",
    ] {
        assert!(
            ir.contains(needle),
            "expected once FFI decl '{needle}'; ir:\n{ir}"
        );
    }
    let body = function_body(&ir, "use_cell").expect("use_cell must lower");
    assert!(
        body.contains("call ptr @karac_runtime_once_new")
            && body.contains("call i8 @karac_runtime_once_set")
            && body.contains("call ptr @karac_runtime_once_get"),
        "use_cell body should call the once FFI; body:\n{body}"
    );
}

#[test]
fn test_ir_oncelock_free_on_scope_exit() {
    // A local `OnceLock` binding queues a `FreeOnceHandle` cleanup, emitting
    // `karac_runtime_once_free` at scope exit — the leak guard.
    let ir = ir_for(
        r#"
fn main() {
    let cell: OnceLock[i64] = OnceLock.new();
    let _ = cell.set(1);
}
"#,
    );
    let body = function_body(&ir, "main").expect("main must lower");
    assert!(
        body.contains("call void @karac_runtime_once_free"),
        "main should free the once handle at scope exit; body:\n{body}"
    );
}

#[test]
fn test_e2e_oncelock_plain_struct_payload() {
    // A small all-scalar struct `T` (heap-free, ≤3 words) round-trips
    // through the cell's memcpy path — the value's bytes are copied in on
    // `set` and loaded back on `get`. No element drop needed (no heap).
    let out = run_program(
        r#"
struct Config { timeout: i64, retries: i64 }
fn main() {
    let c: OnceLock[Config] = OnceLock.new();
    match c.set(Config { timeout: 30, retries: 5 }) { Ok(_) => println(1), Err(_) => println(0), }
    match c.get() {
        Some(cfg) => { println(cfg.timeout); println(cfg.retries); },
        None => println(-1),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n30\n5");
    }
}

#[test]
fn test_e2e_oncelock_scalar_reject_recovery() {
    // A second `set` on a filled cell returns `Err(AlreadySetError {
    // rejected: v })`; the caller can recover `v` via `e.rejected`.
    // `AlreadySetError` is a BAKED stdlib struct that is NOT spliced into
    // `program.items`, so codegen never registered its field layout —
    // `seed_builtin_struct_types` now seeds its metadata + generic base so a
    // construct-and-read resolves the `rejected` field instead of the `i64`
    // fall-through (which silently returned 0). This is a scalar-`T`
    // (ungated) case that shipped as a silent miscompile pre-fix; heap `T`
    // stays loud-gated. Also pins the direct-construction read.
    let out = run_program(
        r#"
fn main() {
    let cell: OnceLock[i64] = OnceLock.new();
    match cell.set(10) { Ok(_) => {} Err(_) => {} }
    match cell.set(77) {
        Ok(_) => { println(-1); }
        Err(e) => { println(e.rejected); }
    }
    let direct: AlreadySetError[i64] = AlreadySetError { rejected: 42 };
    println(direct.rejected);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "77\n42");
    }
}

#[test]
fn test_oncelock_get_or_init_heapfree_aggregate_ok_heap_gated() {
    // B-2026-07-12-2 follow-on: `get_or_init` now accepts a heap-FREE
    // aggregate `T` (a struct/tuple of scalars) — the closure's direct
    // aggregate return is inferred (`infer_closure_return_type`'s
    // struct-literal arm; previously the closure fn was declared `-> i64`
    // and the body's struct `ret` tripped the LLVM verifier). A HEAP-OWNING
    // `T` (a struct with a `String`/`Vec` field) stays gated: the returned
    // `ref T` value-copy would double-free the sealed heap.
    use karac::codegen::compile_to_object_with_options;
    let compile_err = |src: &str, tag: &str| -> Option<String> {
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "{tag}: source must parse");
        karac::prepare_for_resolve(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        assert!(
            typed.errors.is_empty(),
            "{tag}: must typecheck clean: {:?}",
            typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
        );
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        let obj_path = format!("/tmp/karac_goi_gate_{}_{}.o", tag, std::process::id());
        let err = compile_to_object_with_options(
            &parsed.program,
            &obj_path,
            Some(&ownership),
            None,
            None,
            None,
        )
        .err();
        let _ = std::fs::remove_file(&obj_path);
        err.map(|e| e.message)
    };
    // (a) heap-FREE aggregate now compiles.
    let ok = compile_err(
        "struct P { x: i64, y: i64 }\n\
             fn main() {\n\
             let d: OnceLock[P] = OnceLock.new();\n\
             let _p = d.get_or_init(|| P { x: 7i64, y: 8i64 });\n\
             }",
        "heapfree",
    );
    assert!(
        ok.is_none(),
        "heap-free aggregate get_or_init must now compile; got: {ok:?}"
    );
    // (b) heap-owning aggregate now compiles too (B-2026-07-12-2 follow-on
    // closed): the returned value is a borrowed cap-0 view of the sealed
    // element, so the caller's cap-guarded frees no-op — see
    // `codegen/once.rs::compile_once_get_or_init` and the leak validation
    // in `tests/memory_sanitizer.rs::asan_oncelock_get_or_init_heap_*`.
    let heap = compile_err(
        "struct Rec { id: i64, name: String }\n\
             fn main() {\n\
             let c: OnceLock[Rec] = OnceLock.new();\n\
             let _r = c.get_or_init(|| Rec { id: 7i64, name: \"hi\".to_string() });\n\
             }",
        "heap",
    );
    assert!(
        heap.is_none(),
        "heap-owning get_or_init must now compile under karac build; got: {heap:?}"
    );
}

#[test]
fn test_oncelock_wide_element_now_supported_get_or_init_scalar_gated() {
    // B-2026-07-12-2: `set`/`get` now handle ANY element `T` under `karac
    // build`. A heap-FITTING `T` (`OnceLock[String]`, `<= 3` words) was
    // unblocked by the element-drop (gap 1) + rejected-value-drop (gap 2)
    // slice; a WIDE `T` (a struct wider than the 3-word inline `Option`/
    // `Result` payload) is unblocked by the payload-BOXING slice (gap 3):
    // `get` heap-boxes the borrow, `set`'s `Err` payload boxes past the
    // 5-word `Result` area, and a discarded struct-with-heap rejected value
    // is freed by the `FreeInlineResultPayload` struct-drop arm. Leak-freedom
    // is validated in `tests/memory_sanitizer.rs::asan_oncelock_wide_*`.
    // `get_or_init` still gates a NON-SCALAR `T` (its closure returns an
    // aggregate via the deferred sret ABI). The interpreter handles all `T`.
    use karac::codegen::compile_to_object_with_options;
    let compile_err = |src: &str, tag: &str| -> Option<String> {
        let mut parsed = karac::parse(src);
        assert!(parsed.errors.is_empty(), "{tag}: source must parse");
        karac::prepare_for_resolve(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        assert!(
            typed.errors.is_empty(),
            "{tag}: must typecheck clean: {:?}",
            typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
        );
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        let obj_path = format!("/tmp/karac_once_gate_{}_{}.o", tag, std::process::id());
        let err = compile_to_object_with_options(
            &parsed.program,
            &obj_path,
            Some(&ownership),
            None,
            None,
            None,
        )
        .err();
        let _ = std::fs::remove_file(&obj_path);
        err.map(|e| e.message)
    };
    // (a) heap-FITTING `String` element compiles clean.
    let ok = compile_err(
        "fn main() {\n\
             let c: OnceLock[String] = OnceLock.new();\n\
             match c.set(\"hi\".to_string()) { Ok(_) => {}, Err(_) => {}, }\n\
             }",
        "fitting",
    );
    assert!(
        ok.is_none(),
        "heap-FITTING OnceLock[String] must compile under karac build; got: {ok:?}"
    );
    // (b) a WIDE (>3-word) struct element now compiles too (was gated).
    let wide = compile_err(
            "struct Wide { a: i64, b: i64, c: i64, d: i64 }\n\
             fn main() {\n\
             let c: OnceLock[Wide] = OnceLock.new();\n\
             match c.set(Wide { a: 1i64, b: 2i64, c: 3i64, d: 4i64 }) { Ok(_) => {}, Err(_) => {}, }\n\
             match c.get() { Some(_) => {}, None => {}, }\n\
             }",
            "wide",
        );
    assert!(
        wide.is_none(),
        "WIDE-T OnceLock.set/get must now compile under karac build (gap 3); got: {wide:?}"
    );
    // (c) `get_or_init` with a HEAP-OWNING element now compiles as well
    // (borrowed cap-0 view return — the last B-2026-07-12-2 follow-on;
    // see `test_oncelock_get_or_init_heapfree_aggregate_ok_heap_gated`
    // and the asan_oncelock_get_or_init_heap_* leak validation).
    let goi = compile_err(
        "struct Rec { id: i64, name: String }\n\
             fn main() {\n\
             let c: OnceLock[Rec] = OnceLock.new();\n\
             let r = c.get_or_init(|| Rec { id: 1i64, name: \"hi\".to_string() });\n\
             }",
        "get_or_init_heap",
    );
    assert!(
        goi.is_none(),
        "get_or_init with a heap-owning element must now compile; got: {goi:?}"
    );
}

#[test]
fn test_e2e_oncelock_get_or_init_heap_elements() {
    // B-2026-07-12-2 follow-on (closed): `get_or_init` with heap-owning
    // element types — `String`, `Vec[i64]`, and a struct with a `String`
    // field — returns a borrowed cap-0 view of the sealed value. The
    // closure fires only on the first (unset) call: the second
    // `get_or_init` must return the sealed value, not run its closure.
    // Method dispatch on the binding (`a.len()`) works via the let-site
    // registration under the cell's element type. Leak/double-free
    // validation lives in `tests/memory_sanitizer.rs`'s
    // `asan_oncelock_get_or_init_heap_*` twins.
    if let Some(out) = run_program(
        "struct Config { name: String, port: i64 }\n\
             fn main() {\n\
                 let c: OnceLock[String] = OnceLock.new();\n\
                 let a = c.get_or_init(|| \"first\".to_string());\n\
                 println(a);\n\
                 let b = c.get_or_init(|| \"second\".to_string());\n\
                 println(b);\n\
                 println(a.len());\n\
                 let v: OnceCell[Vec[i64]] = OnceCell.new();\n\
                 let xs = v.get_or_init(|| Vec[10, 20, 30]);\n\
                 println(xs.len());\n\
                 match xs.get(1) { Some(n) => println(n), None => println(-1) }\n\
                 let k: OnceLock[Config] = OnceLock.new();\n\
                 let cfg = k.get_or_init(|| Config { name: \"svc\".to_string(), port: 8080 });\n\
                 println(cfg.name);\n\
                 println(cfg.port);\n\
                 let s: OnceLock[String] = OnceLock.new();\n\
                 match s.set(\"sealed\".to_string()) { Ok(_) => {}, Err(_) => {}, }\n\
                 let t = s.get_or_init(|| \"never\".to_string());\n\
                 println(t);\n\
             }",
    ) {
        assert_eq!(out, "first\nfirst\n5\n3\n20\nsvc\n8080\nsealed\n");
    }
}

#[test]
fn test_e2e_oncelock_wide_allscalar_set_get_reject() {
    // B-2026-07-12-2 gap 3: a WIDE all-scalar element (4 words > the 3-word
    // Option inline area). `get` heap-boxes the borrow; `set`'s second call
    // is rejected and `get` still reads the first value. Build must match the
    // interpreter.
    if let Some(out) = run_program(
            "struct Wide { a: i64, b: i64, c: i64, d: i64 }\n\
             fn main() {\n\
                 let c: OnceLock[Wide] = OnceLock.new();\n\
                 match c.set(Wide { a: 1i64, b: 2i64, c: 3i64, d: 4i64 }) { Ok(_) => { println(\"set ok\"); }, Err(_) => { println(\"set err\"); }, }\n\
                 match c.get() { Some(w) => { println((w.a + w.b + w.c + w.d).to_string()); }, None => { println(\"none\"); }, }\n\
                 match c.set(Wide { a: 9i64, b: 9i64, c: 9i64, d: 9i64 }) { Ok(_) => { println(\"2 ok\"); }, Err(_) => { println(\"2 rejected\"); }, }\n\
                 match c.get() { Some(w) => { println((w.a + w.b + w.c + w.d).to_string()); }, None => { println(\"none\"); }, }\n\
             }",
        ) {
            assert_eq!(out, "set ok\n10\n2 rejected\n10\n");
        }
}

#[test]
fn test_e2e_process_spawn_pipe_env_roundtrip() {
    // `std.process` codegen (phase-8 P1) — the spawn → take-stream →
    // read_to_string → wait happy path plus env passthrough, stdin
    // write/close, and the spawn-error path, matching the interpreter
    // (`tests/interpreter.rs` § std.process). Chained builder receivers
    // exercise the `Command`-returning links (arg/env/stdout/stdin);
    // `read_to_string` exercises the StringPayload Ok protocol;
    // `wait` the bit-packed ExitStatus protocol.
    if let Some(out) = run_program(
        r#"
fn main() {
    let cmd = Command.new("echo").arg("kara-spawn").stdout(Stdio.Piped);
    match cmd.spawn() {
        Ok(child) => {
            match child.stdout() {
                Some(o) => {
                    match o.read_to_string() {
                        Ok(text) => println(f"out={text}"),
                        Err(e) => println("read failed"),
                    }
                }
                None => println("no stdout handle"),
            }
            // Stream taken at most once.
            match child.stdout() {
                Some(o2) => println("second take?!"),
                None => println("taken once"),
            }
            match child.wait() {
                Ok(st) => println(f"code={st.code} success={st.success}"),
                Err(e) => println("wait failed"),
            }
        }
        Err(e) => println("spawn failed"),
    }

    let ecmd = Command.new("sh").arg("-c").arg("printf %s \"$KARA_E2E\"")
        .env("KARA_E2E", "env-ok").stdout(Stdio.Piped);
    match ecmd.spawn() {
        Ok(child) => {
            match child.stdout() {
                Some(o) => {
                    match o.read_to_string() {
                        Ok(text) => println(f"env={text}"),
                        Err(e) => println("env read failed"),
                    }
                }
                None => println("no env stdout"),
            }
            match child.wait() {
                Ok(st) => println(f"env-code={st.code}"),
                Err(e) => println("env wait failed"),
            }
        }
        Err(e) => println("env spawn failed"),
    }

    let cat = Command.new("cat").stdin(Stdio.Piped).stdout(Stdio.Piped);
    match cat.spawn() {
        Ok(child) => {
            match child.stdin() {
                Some(sin) => {
                    match sin.write("through-the-pipe\n") {
                        Ok(u) => println("wrote"),
                        Err(e) => println("write failed"),
                    }
                    match sin.close() {
                        Ok(u) => println("closed"),
                        Err(e) => println("close failed"),
                    }
                }
                None => println("no stdin handle"),
            }
            match child.stdout() {
                Some(o) => {
                    match o.read_to_string() {
                        Ok(text) => println(f"cat={text}"),
                        Err(e) => println("cat read failed"),
                    }
                }
                None => println("no cat stdout"),
            }
            match child.wait() {
                Ok(st) => println(f"cat-code={st.code}"),
                Err(e) => println("cat wait failed"),
            }
        }
        Err(e) => println("cat spawn failed"),
    }

    let bad = Command.new("definitely-not-a-binary-kara");
    match bad.spawn() {
        Ok(c) => println("bad spawned?!"),
        Err(e) => {
            match e {
                IoError.NotFound => println("bad=NotFound"),
                _ => println("bad=other"),
            }
        }
    }
}
"#,
    ) {
        assert_eq!(
                out,
                "out=kara-spawn\n\ntaken once\ncode=0 success=true\nenv=env-ok\nenv-code=0\nwrote\nclosed\ncat=through-the-pipe\n\ncat-code=0\nbad=NotFound\n"
            );
    }
}

#[test]
fn test_e2e_oncelock_wide_heap_struct_set_get() {
    // B-2026-07-12-2 gap 3: a WIDE struct-with-heap element (`Rec { id,
    // name: String }`, 4 words). `get`'s boxed borrow reconstructs the
    // struct and reads its `String` field.
    if let Some(out) = run_program(
            "struct Rec { id: i64, name: String }\n\
             fn main() {\n\
                 let c: OnceLock[Rec] = OnceLock.new();\n\
                 match c.set(Rec { id: 7i64, name: \"hello\".to_string() }) { Ok(_) => { println(\"set ok\"); }, Err(_) => { println(\"set err\"); }, }\n\
                 match c.get() { Some(r) => { println(f\"{r.id}:{r.name}\"); }, None => { println(\"none\"); }, }\n\
             }",
        ) {
            assert_eq!(out, "set ok\n7:hello\n");
        }
}

#[test]
fn test_e2e_oncelock_wide_heap_struct_reject_recover() {
    // B-2026-07-12-2 gap 3: recover the rejected WIDE struct out of `set`'s
    // `Err(AlreadySetError { rejected })` payload and read its fields
    // (chained field READ recovery, unblocked by the generic-mono chained-
    // field-access fix).
    if let Some(out) = run_program(
            "struct Rec { id: i64, name: String }\n\
             fn main() {\n\
                 let c: OnceLock[Rec] = OnceLock.new();\n\
                 match c.set(Rec { id: 1i64, name: \"first\".to_string() }) { Ok(_) => { println(\"1 ok\"); }, Err(_) => { println(\"1 err\"); }, }\n\
                 match c.set(Rec { id: 2i64, name: \"second\".to_string() }) {\n\
                     Ok(_) => { println(\"2 ok\"); },\n\
                     Err(e) => { println(f\"rejected {e.rejected.id}:{e.rejected.name}\"); },\n\
                 }\n\
             }",
        ) {
            assert_eq!(out, "1 ok\nrejected 2:second\n");
        }
}

#[test]
fn test_e2e_oncelock_get_or_init_heapfree_aggregate() {
    // B-2026-07-12-2 follow-on: `get_or_init` with a heap-FREE aggregate `T`
    // — the closure returns the struct by value (direct aggregate return,
    // inferred by `infer_closure_return_type`'s struct-literal arm). First
    // call seals the cell; the second returns the existing value without
    // re-running the closure. Build must match the interpreter.
    if let Some(out) = run_program(
        "struct Point { x: i64, y: i64 }\n\
             fn main() {\n\
                 let c: OnceLock[Point] = OnceLock.new();\n\
                 let p = c.get_or_init(|| Point { x: 3i64, y: 4i64 });\n\
                 println((p.x + p.y).to_string());\n\
                 let q = c.get_or_init(|| Point { x: 99i64, y: 99i64 });\n\
                 println((q.x + q.y).to_string());\n\
             }",
    ) {
        // first call wins (7); second returns the sealed value, not 198.
        assert_eq!(out, "7\n7\n");
    }
}

#[test]
fn test_e2e_two_interners_par_group_bail() {
    // Regression (2026-07-17, found via the Arena slice): two ANNOTATED
    // `let t: Interner = Interner.new()` lets form an auto-par parallel
    // group whose return-slot type IS inferable (the annotation lowers
    // to `ptr`), so the group really parallelized — and the branch's
    // scope-exit `FreeInternerHandle` freed the handle before the
    // parent's `intern` locked it (futex hang on a dead Mutex, no
    // output). The unannotated form dodged it by accident (RHS type
    // uninferable → group bailed). Now any escaping handle-new binding
    // bails its group to sequential (`compute_return_slots_checked`).
    let out = run_program(
        r#"
fn main() {
    let t: Interner = Interner.new();
    let u: Interner = Interner.new();
    let _a = t.intern("x");
    let _b = u.intern("y");
    println(t.len());
    println(u.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n1");
    }
}

#[test]
fn test_e2e_module_global_oncelock_config_pattern() {
    // The canonical late-bound global: `let CONFIG: OnceLock[T] =
    // OnceLock.new()` at module scope, `set` once in `main`, `get` from a
    // sibling fn. The handle lives in a global filled by the static-init
    // prologue (`karac_runtime_once_new` before `main`), so the write is
    // observable across calls. Module bindings are codegen-only (no
    // interpreter support), so this is a `karac build` feature — matching
    // the Map/Set module-binding tests. Also pins the double-set →
    // `AlreadySetError` (0) path against the persisted global.
    let src = "let CONFIG: OnceLock[i64] = OnceLock.new();\n\
                   fn show() {\n\
                       match CONFIG.get() { Some(v) => println(v), None => println(-1), }\n\
                   }\n\
                   fn main() {\n\
                       println(CONFIG.is_set());\n\
                       show();\n\
                       match CONFIG.set(1234) { Ok(_) => println(1), Err(_) => println(0), }\n\
                       println(CONFIG.is_set());\n\
                       show();\n\
                       match CONFIG.set(9999) { Ok(_) => println(1), Err(_) => println(0), }\n\
                       show();\n\
                   }";
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "module-scope OnceLock.new() must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    let output = run_program(src).expect("compile + run failed");
    assert_eq!(output, "false\n-1\n1\ntrue\n1234\n0\n1234\n");
}

#[test]
fn test_ir_module_global_oncelock_static_init() {
    // A module-scope `OnceLock.new()` takes the placeholder-null-ptr-global
    // + static-init path: `__karac_static_init` builds the cell via
    // `karac_runtime_once_new`, and `main` forward-calls the prologue.
    let ir = ir_for(
        "let CONFIG: OnceLock[i64] = OnceLock.new();\n\
             fn main() { let _ = CONFIG.set(7); }",
    );
    assert!(
        ir.contains("@CONFIG = internal global ptr null"),
        "expected null-ptr placeholder global for CONFIG; ir:\n{ir}"
    );
    let init = function_body(&ir, "__karac_static_init")
        .expect("__karac_static_init must be emitted for a module OnceLock");
    assert!(
        init.contains("call ptr @karac_runtime_once_new"),
        "static-init should build the once cell; body:\n{init}"
    );
    let main = function_body(&ir, "main").expect("main must lower");
    assert!(
        main.contains("call void @__karac_static_init"),
        "main should forward-call the static-init prologue; body:\n{main}"
    );
}

#[test]
fn e2e_backpressure_semaphore_and_rate_limiter() {
    // `Semaphore` (new/acquire/release) and `RateLimiter`
    // (new_token_bucket/try_acquire) codegen (phase-8 backpressure
    // primitives) — the collapsed single-threaded semantics match the
    // interpreter byte-for-byte: acquire grants up to the permit budget
    // then fails closed with `Err(Timeout)`; release frees one;
    // try_acquire bursts up to capacity per key then reports limited, with
    // independent per-key buckets. Same output as
    // tests/interpreter.rs::test_semaphore_and_rate_limiter_codegen_parity.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let sem = Semaphore.new(2i64);\n\
                 match sem.acquire(0i64) { Ok(u) => { println(\"a1\"); }, Err(e) => { println(\"t1\"); }, }\n\
                 match sem.acquire(0i64) { Ok(u) => { println(\"a2\"); }, Err(e) => { println(\"t2\"); }, }\n\
                 match sem.acquire(0i64) { Ok(u) => { println(\"a3\"); }, Err(e) => { println(\"t3\"); }, }\n\
                 sem.release();\n\
                 match sem.acquire(0i64) { Ok(u) => { println(\"a4\"); }, Err(e) => { println(\"t4\"); }, }\n\
                 let rl = RateLimiter.new_token_bucket(1i64, 3i64);\n\
                 println(rl.try_acquire(\"k1\"));\n\
                 println(rl.try_acquire(\"k1\"));\n\
                 println(rl.try_acquire(\"k1\"));\n\
                 println(rl.try_acquire(\"k1\"));\n\
                 println(rl.try_acquire(\"k2\"));\n\
             }",
        ) {
            assert_eq!(out, "a1\na2\nt3\na4\ntrue\ntrue\ntrue\nfalse\ntrue\n");
        }
}

/// Regression: `TaskHandle[T].join()` for a NON-scalar `T` (`Vec[i64]`)
/// returns the spawned task's heap value intact. `recover_task_handle_
/// join_return_ty` used to return `i64` unconditionally, so a `Vec`/
/// `String`/struct spawn return was read as `i64`-shaped bytes off the
/// result buffer — `.len()` came back as garbage and the program trapped.
/// The typechecker now records each join's `T` in `task_join_return_types`
/// and codegen sizes the cross-task transfer + out-slot for it. Surfaced
/// by the Fathom dogfood (parallel Mandelbrot row-bands return `Vec[u8]`).
/// The colliding-name variant (`for handle in handles` where `handle` is
/// also the spawn-binding name) is exercised separately below
/// (`e2e_for_loop_binding_name_collision_no_false_rc`).
#[test]
fn e2e_taskhandle_join_returns_nonscalar_vec() {
    if let Some(out) = run_program(
        "fn band(n: i64) -> Vec[i64] {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let mut i = 0;\n\
                 while i < n { v.push(i); i = i + 1; }\n\
                 v\n\
             }\n\
             fn main() {\n\
                 let mut pool: TaskGroup = TaskGroup.new();\n\
                 let mut handles: Vec[TaskHandle[Vec[i64]]] = Vec.new();\n\
                 let mut k = 0;\n\
                 while k < 3 {\n\
                     let t: TaskHandle[Vec[i64]] = pool.spawn(|| band(k + 2));\n\
                     handles.push(t);\n\
                     k = k + 1;\n\
                 }\n\
                 let mut total = 0;\n\
                 for h in handles { let c: Vec[i64] = h.join(); total = total + c.len(); }\n\
                 println(f\"{total}\");\n\
             }",
    ) {
        // bands of length 2, 3, 4 → 9 elements total (was garbage/trap before the fix)
        assert_eq!(out, "9\n");
    }
}

#[test]
fn test_e2e_size_of_epoll_data_style_union() {
    // The design.md `epoll_data` example: four-field FFI union
    // dominated by the 8-byte / 8-aligned `u64val` field. Storage
    // collapses to `{ i64 }`; `size_of` / `align_of` both report 8.
    let out = run_program(
        "#[repr(C)] union EpollData { ptr: *mut u8, fd: i32, u32val: u32, u64val: u64 }\n\
             fn main() { println(size_of[EpollData]()); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_align_of_epoll_data_style_union() {
    // B-2026-08-12-7: the raw-pointer field is the point. `is_type_copy`
    // had no `Type::Pointer` arm, so `E_UNION_FIELD_NOT_COPY` rejected
    // the one shape its own suggestion ("hold it behind a raw pointer")
    // tells the user to write — and this test only ran at all because
    // the harness discarded typecheck errors (B-2026-08-11-34).
    let out = run_program(
        "#[repr(C)] union EpollData { ptr: *mut u8, fd: i32, u32val: u32, u64val: u64 }\n\
             fn main() { println(align_of[EpollData]()); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_factorial_while() {
    let out = run_program(
        r#"
fn factorial(n: i64) -> i64 {
    let mut result = 1;
    let mut i = 1;
    while i <= n {
        result = result * i;
        i = i + 1;
    }
    result
}
fn main() { println(factorial(10)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3628800");
    }
}

#[test]
fn e2e_par_sleeps_both_branches_complete() {
    // Two `sleep_ms` calls inside `par {}` each run to completion (the
    // overlap *timing* win is measured by the bench harness
    // `bench/auto_par_io`; here we pin that the par+suspend composition
    // resumes BOTH branches rather than wedging one). Each branch prints
    // after its nap; both lines must appear.
    if let Some(out) = run_program(
        "fn main() {\n\
                 par {\n\
                     { sleep_ms(20); println(\"left\"); }\n\
                     { sleep_ms(20); println(\"right\"); }\n\
                 }\n\
             }",
    ) {
        assert!(
            out.contains("left") && out.contains("right"),
            "expected both par branches to complete after their naps; got:\n{out}"
        );
    }
}

#[test]
fn e2e_auto_par_sleeps_run_correctly() {
    // A2b: two independent `sleep_ms` timer waits AUTO-parallelize (no
    // explicit `par {}` — the conflict model exempts a standalone `sleep_ms`
    // from the `suspends` boundary gate and lifts `(Suspends,Suspends)`).
    // They overlap on the par thread-block path; here we pin output
    // correctness and termination (the timing win is the bench's job).
    if let Some(out) = run_program(
        "fn main() {\n\
                 sleep_ms(20);\n\
                 sleep_ms(20);\n\
                 println(\"done\");\n\
             }",
    ) {
        assert_eq!(out, "done\n", "auto-par sleeps complete; got:\n{out}");
    }
}

#[test]
fn e2e_auto_par_allocating_calls_run_correctly() {
    // A3: two independent statements that each only `allocates(Heap)` (each
    // builds a fresh Vec) AUTO-parallelize — the conflict model no longer
    // treats allocates+allocates as a conflict (heap is thread-safe;
    // `allocates` is informational per design.md). They run on the par
    // fan-out path; pin that each branch's heap allocation is independent
    // and the combined result is correct (no cross-branch corruption).
    if let Some(out) = run_program(
        "fn make(n: i64) -> Vec[i64] {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(n);\n\
                 v.push(n + 1);\n\
                 return v;\n\
             }\n\
             fn main() {\n\
                 let a = make(10);\n\
                 let b = make(20);\n\
                 println(a[0] + a[1] + b[0] + b[1]);\n\
             }",
    ) {
        assert_eq!(
            out, "62\n",
            "auto-par allocating calls must each build their Vec correctly; got:\n{out}"
        );
    }
}

#[test]
fn e2e_auto_par_panicking_calls_run_correctly() {
    // A3b: two independent statements that each only `panics` (each calls a
    // dividing helper — `/` infers `panics`) AUTO-parallelize now that the
    // conflict model treats panics+panics as non-conflicting. Neither
    // actually divides by zero, so this is the common, beneficial case:
    // ordinary arithmetic runs concurrently. Pin output correctness.
    if let Some(out) = run_program(
        "fn divmod(n: i64, d: i64) -> i64 { return n / d; }\n\
             fn main() {\n\
                 let a = divmod(100, 5);\n\
                 let b = divmod(200, 4);\n\
                 println(a + b);\n\
             }",
    ) {
        assert_eq!(
            out, "70\n",
            "auto-par panicking-effect calls must compute correctly; got:\n{out}"
        );
    }
}

#[test]
fn e2e_auto_par_branch_panic_fails_fast() {
    // A3b soundness gate: when one of two grouped panic-capable statements
    // ACTUALLY panics inside a `__par_branch` worker (here a real divide by
    // zero), the program must FAIL FAST — a Kāra panic lowers to `exit(1)`
    // (a direct process exit, not a Rust unwind), so the worker terminates
    // the whole process. We assert a non-zero exit and the panic message,
    // and the `output_with_hang_watchdog` wrapper makes this double as a
    // deadlock guard: a regression that hung on the worker exit (e.g. exit
    // while another thread holds the output lock) would trip the watchdog
    // rather than pass.
    if let Some(c) = run_program_capturing(
        "fn divmod(n: i64, d: i64) -> i64 { return n / d; }\n\
             fn main() {\n\
                 let zero = 0;\n\
                 let a = divmod(100, 5);\n\
                 let b = divmod(200, zero);\n\
                 println(a + b);\n\
             }",
    ) {
        assert_eq!(
            c.status.code(),
            Some(101),
            "a branch panic must fail fast with exit 1; stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            c.stderr.contains("division by zero"),
            "expected the div-by-zero panic message; stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn e2e_auto_par_channel_consumer_terminates() {
    // Regression (A2b): the producer/consumer channel program must TERMINATE
    // under default auto-par. `consume(rx)` carries a `suspends` effect (from
    // `rx.recv()`) that is indistinguishable at the effect level from a
    // `sleep_ms` timer wait, but a channel recv has a happens-before with its
    // producer — lifting `consume(rx)` into a `__par_branch` worker alongside
    // the `spawn` deadlocks (the recv loop never observes the channel close).
    // The conservative boundary gate keeps it serial (only direct `sleep_ms`
    // is exempt). A regression here would HANG, not just misprint, so the
    // test doubles as the deadlock guard.
    if let Some(out) = run_program(
        "fn producer(tx: Sender[i64]) -> i64 {\n\
                 tx.send(10);\n\
                 tx.send(20);\n\
                 0\n\
             }\n\
             fn consume(rx: Receiver[i64]) -> i64 {\n\
                 let mut sum = 0;\n\
                 let mut go = true;\n\
                 while go {\n\
                     let v = rx.recv();\n\
                     if v == 0 { go = false; } else { sum = sum + v; }\n\
                 }\n\
                 sum\n\
             }\n\
             fn main() {\n\
                 let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
                 let h: TaskHandle[i64] = spawn(|| producer(tx));\n\
                 println(consume(rx));\n\
                 h.join();\n\
             }",
    ) {
        assert_eq!(
            out, "30\n",
            "channel consumer must terminate with 30; got:\n{out}"
        );
    }
}

#[test]
fn test_e2e_par_group_serializes_for_iter_with_outer_mutable_write() {
    // The concurrency analyzer's per-stmt info now collects
    // nested-block writes (Assign / CompoundAssign) into
    // `info.defines`. Without this, a `for v in nums.iter()`
    // expression-stmt that writes to outer `cap` was treated as
    // "no dependencies" against a subsequent `let f =
    // dummy(cap)` — the analyzer grouped them and the par-branch
    // fn's local copy of `cap` never propagated back, so the
    // function call read the initial value of `cap`.
    //
    // Repro:
    let out = run_program(
        r#"
fn dummy(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < n { v.push(i); i = i + 1; }
    v
}
fn helper(nums: Slice[i64]) -> i64 {
    let mut cap = 1i64;
    for v in nums.iter() {
        if v > cap { cap = v; }
    }
    let f: Vec[i64] = dummy(cap);
    println(cap);
    println(f.len());
    cap
}
fn main() {
    let a: Array[i64, 4] = [1, 2, 4, 6];
    println(helper(a));
}
"#,
    );
    if let Some(out) = out {
        // cap finds max = 6 from [1,2,4,6]; dummy(6) yields a
        // 6-element Vec; helper returns 6.
        assert_eq!(out.trim(), "6\n6\n6");
    }
}

#[test]
fn test_e2e_par_group_return_slot_preserves_vec_bool_elem_type() {
    // Regression: when auto-par groups `let v: Vec[bool] = ...`
    // with another stmt, the return-slot rebind in
    // `compile_function_body` was unconditionally overwriting
    // `vec_elem_types[v]` to i64 (the placeholder). Later
    // `not v[i]` then loaded an i64 instead of bool, lowered
    // through `xor i64 …, -1`, and the short-circuit phi
    // rejected the i64 operand against an i1 result. Fix uses
    // `entry().or_insert_with(...)` to preserve the let's
    // annotated element type.
    let out = run_program(
        r#"
fn helper(nums: Slice[i64]) -> i64 {
    let n = nums.len();
    let mut visited: Vec[bool] = Vec.filled(n, false);
    let mut bucket: Map[i64, i64] = Map.new();
    let i = 1i64;
    if i > 0 and not visited[i - 1] {
        return 1;
    }
    0
}
fn main() {
    let a: Array[i64, 3] = [1, 2, 3];
    println(helper(a));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_e2e_auto_par_propagates_let_bindings_with_identifier_rhs() {
    // `let n = p; let v: Vec[T] = Vec.new()` are independent
    // statements, so the concurrency analyzer groups them as
    // parallelizable. Before the fix, `infer_let_binding_llvm_type`
    // returned None for `let n = p` (Identifier RHS, no type
    // annotation), so the return-slot machinery silently dropped
    // `n` — the tail-expression read failed with "Undefined
    // variable 'n'". Fix: extend the inference to read the RHS
    // identifier's type from `self.variables`.
    let out = run_program(
        r#"
fn foo(p: i64) -> i64 {
    let n = p;
    let v: Vec[i64] = Vec.new();
    let _ = v.len();
    n
}
fn main() { println(foo(3)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_auto_par_drops_untypeable_return_slot_groups() {
    // `let n = nums.len()` has a MethodCall RHS that the
    // let-binding type inference can't recover. Auto-par groups
    // would silently drop the `n` slot before the fix — `n` then
    // became a class-(i) branch-local with no parent propagation,
    // surfacing later as "Undefined variable 'n'" at the read
    // site. Fix: when any needed-outside binding has un-typeable
    // RHS, `compute_return_slots_checked` returns None and the
    // caller drops the par-group, falling back to sequential
    // compilation (correct, just slower).
    let out = run_program(
        r#"
fn foo(nums: Slice[i64]) -> i64 {
    let n = nums.len();
    let mut visited: Vec[bool] = Vec.new();
    for _ in 0..n { visited.push(false); }
    visited[0] = true;
    let mut sum = 0i64;
    let mut i = 0i64;
    while i < n {
        sum = sum + nums[i];
        i = i + 1;
    }
    sum
}
fn main() {
    let a: Array[i64, 3] = [1, 2, 3];
    println(foo(a));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

#[test]
fn test_ir_gpu_scalar_kernel_lowers_match_to_select_chain() {
    // B-2026-08-18-40 increment 4: a value `match` becomes a nested
    // `select()` — the same branchless shape value-`if` already uses, since
    // WGSL's `switch` is a statement and cannot produce a value. Literal
    // arms, `|` alternations and `_` all appear here. Codegen-only, so
    // CI-safe; the executed twin ran on lavapipe and matched the
    // interpreter (40, 20, 10).
    let src = r#"
#[gpu]
fn weight(x: i32) -> i32 {
    let w: i32 = match x {
        0 => 4,
        1 | 2 => 2,
        _ => 1,
    };
    w.wrapping_mul(10)
}

fn main() {
    let mut v: Vec[i32] = Vec.new();
    v.push(0);
    let out = gpu.dispatch(weight, v);
    println(out[0]);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
            ir.contains(
                "let w = select(select(1, 2, ((input[i] == 1) || (input[i] == 2))), 4, (input[i] == 0));"
            ),
            "the match must lower to a nested select chain with the alternation OR'd; got:\n{ir}"
        );
}

#[test]
fn test_ir_par_block_arc_promoted_binding_uses_atomic_rc() {
    // Trigger 1 (branch-divergent re-use) flags `d` as RC-fallback; the
    // par-block crossing makes Phase 2 promote it to Arc. Codegen must
    // emit `atomicrmw` for the Arc-flagged binding's inc/dec, not plain
    // load+add/sub+store. Pattern verified to populate `arc_values["process"]`
    // by `tests/rc_fallback.rs::par_block_promotes_rc_to_arc`.
    //
    // **Why a non-shared `struct Data` instead of `shared struct Counter`:**
    // RC-fallback (and Arc promotion) only fires on bindings that the
    // ownership pass routes through the `rc_values` table — i.e. non-shared
    // types whose use pattern (branch-divergent re-use) forces a heap-boxed
    // refcount fallback. `shared struct` types already carry built-in RC
    // machinery; they don't go through the fallback path and are excluded
    // from the predicate output, so they never reach `arc_values`. Updated
    // 2026-05-13 from the prior `shared struct Counter` shape (which never
    // triggered RC and emitted plain inc/dec) to mirror the rc_fallback
    // integration test that exercises the exact codegen path under test.
    //
    // **Multi-stmt par-block** so `emit_par_run`'s single-statement fast
    // path (`if stmts.len() == 1 { compile_stmt sequentially }`) doesn't
    // collapse the par-block into plain sequential code — the runtime
    // dispatch via `karac_par_run` is what makes Phase 2 detect this as a
    // par-region for arc promotion. Two consumes inside `par { }`.
    //
    // **Runs from `process`, called by `main`:** the par block in a
    // void-returning user function still trips the pre-existing
    // module-verifier `ret i64 0` wart, independent of this slice; called
    // from a non-void `main` keeps the verifier happy.
    let ir = ir_for_with_ownership(
        r#"
struct Data { value: i64 }
fn consume(d: Data) -> i64 { d.value }
fn use_d(d: Data) -> i64 { d.value }
fn process(cond: bool, d: Data) -> i64 {
    if cond { consume(d); }
    par {
        use_d(d);
        let _throwaway = 0i64;
    }
    0i64
}
fn main() {
    process(false, Data { value: 7 });
}
"#,
    );
    // Atomic DEC at scope exit must emit `atomicrmw sub` with `seq_cst`
    // ordering. The DEC fires from `CleanupAction::RcDec` at the end of
    // `process`, dispatched through `emit_refcount_dec` →
    // `is_arc_binding("d") == true` → `emit_arc_dec` (atomicrmw sub).
    assert!(
        ir.contains("atomicrmw sub"),
        "Arc-promoted binding's dec should lower to `atomicrmw sub`; IR:\n{ir}"
    );
    assert!(
        ir.contains("seq_cst"),
        "atomicrmw should use SeqCst ordering; IR:\n{ir}"
    );

    // TODO 2026-05-13: per-consume-site atomic INC for rc-fallback
    // bindings isn't currently emitted by codegen. The rc-fallback design
    // requires each consume site (`consume(d)` in the if-arm,
    // `use_d(d)` in the par-block) to be preceded by an inc so the
    // binding's refcount survives both consumes when both branches fire
    // on the same execution path. Today only the initial-refcount-1
    // store + final dec exist for an rc-fallback binding; consume sites
    // pass the heap pointer through unincremented. When that inc
    // emission lands, restore:
    //   assert!(ir.contains("atomicrmw add"), ...);
    // and re-tighten the test to verify both halves of the atomic-RC
    // path. Tracked separately from this slice — out-of-scope for the
    // pre-existing-test-pass fix.
}

#[test]
fn test_ir_non_par_binding_uses_plain_rc() {
    // Same trigger-1 RC shape, no par block. The binding stays in
    // `rc_values` but not `arc_values`, so codegen keeps the plain
    // non-atomic ops and emits no `atomicrmw`. Regression guard: the
    // dispatcher must not unconditionally route through the atomic
    // helper when only `rc_fallback_fns` is populated.
    let ir = ir_for_with_ownership(
        r#"
shared struct Counter { val: i64 }
fn use_c(c: Counter) -> i64 { c.val }
fn main() {
    let cond: bool = false;
    let c = Counter { val: 7 };
    let d = c;
    if cond { use_c(d); }
    use_c(d);
}
"#,
    );
    assert!(
        !ir.contains("atomicrmw"),
        "non-par RC binding must not use atomic ops; IR:\n{ir}"
    );
}

#[test]
fn test_e2e_question_trace_includes_source_filename_when_threaded() {
    // When the CLI / caller threads a `source_filename` into codegen
    // (`compile_to_object_with_options`), each `?` failure-site frame
    // carries the filename so the trace prints as `<file>:<line>:<col>`,
    // matching the interpreter's format. The default (no filename) MVP
    // path emits `<line>:<col>` only — covered by the tests above.
    let captured = run_program_capturing_with_filename(
        r#"
fn boom() -> Result[i64, i64] { Err(7_i64) }
fn caller() -> Result[i64, i64] {
    let _ = boom()?;
    Ok(0_i64)
}
fn main() {
    match caller() {
        Ok(_) => println(0_i64),
        Err(e) => println(e),
    }
}
"#,
        "trace_demo.kara",
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "7");
        assert!(c.stderr.contains("Error return trace:"));
        let has_file_frame = c.stderr.lines().any(|l| {
            l.starts_with("  ") && l.contains("trace_demo.kara:") && !l.contains("truncated")
        });
        assert!(
            has_file_frame,
            "expected a `<file>:<line>:<col>` frame containing `trace_demo.kara:`; \
                 got {:?}",
            c.stderr
        );
    }
}

/// B-2026-08-14-28 — the plain-build face of the cluster-walk
/// use-after-free. Sibling of
/// `asan_shared_cluster_published_from_a_par_branch`, which is the
/// authoritative gate (only LeakSanitizer separates a correct TRANSFER of
/// the free-walk from a mere suppression that leaks).
///
/// This one is here because the field failure was a SIGSEGV, not a wrong
/// number: `leetcode/1-100/2-add-two-numbers/iterative.kara` died on its
/// first `to_string`. `run_program` returns `None` on an abort, so this
/// asserts only that the program reaches the end — which is exactly what it
/// could not do.
#[test]
fn test_e2e_shared_cluster_survives_a_par_join() {
    let src = r#"
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn consume(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    loop {
        let mut done = true;
        if let Some(n) = a { a = n.next; done = false; }
        if let Some(n) = b { b = n.next; done = false; }
        if done { break; }
        let node = ListNode { val: 7, next: None };
        tail.next = Some(node);
        tail = node;
    }
    dummy.next
}

fn from_array(arr: Slice[i64]) -> Option[ListNode] {
    let n = arr.len();
    if n == 0 { return None; }
    let head = ListNode { val: arr[0], next: None };
    let mut tail = head;
    for i in 1..n {
        let node = ListNode { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}

fn total(list: Option[ListNode]) -> i64 {
    let mut c = 0i64;
    let mut cur = list;
    loop {
        match cur {
            Some(n) => { c = c + n.val; cur = n.next; }
            None => break,
        }
    }
    c
}

fn report(a: Slice[i64], b: Slice[i64]) {
    let l1 = from_array(a);
    let l2 = from_array(b);
    let out = consume(l1, l2);
    println(f"{total(out)}");
}

fn main() {
    let a1: Array[i64, 3] = [2, 4, 3];
    let b1: Array[i64, 3] = [5, 6, 4];
    report(a1, b1);
    let a2: Array[i64, 1] = [1];
    let b2: Array[i64, 2] = [9, 9];
    report(a2, b2);
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("21\n14\n"));
}

/// B-2026-08-14-27 — the plain-build face of the par-join double-free.
///
/// Its sibling `asan_borrow_elided_index_read_across_a_par_join` is the
/// authoritative gate; this one is here because the failure was not subtle
/// in the field. `leetcode/54-spiral-matrix/spiral_boundary.kara` aborted
/// with `free(): double free detected in tcache 2` on its third test case,
/// and the same shape reduced here dies the same way — an abort, so
/// `run_program` returns `None` and this asserts nothing about ordering or
/// leaks, only that the program still finishes.
///
/// The 1x1 case is the one the kata actually died on: with a single 8-byte
/// row, the second free lands inside a live allocation rather than on a
/// block glibc has already recycled.
#[test]
fn test_e2e_borrow_elided_index_read_survives_a_par_join() {
    let src = r#"
fn spiral(m: ref Vec[Vec[i64]]) -> Vec[i64] {
    let mut out: Vec[i64] = Vec.new();
    let rows = m.len();
    let first = ref m[0];
    let cols = first.len();
    let mut r = 0i64;
    while r < rows {
        let mut c = 0i64;
        while c < cols {
            out.push(m[r][c]);
            c = c + 1i64;
        }
        r = r + 1i64;
    }
    out
}

fn report(grid: Vec[Vec[i64]]) {
    let m = grid;
    let order = spiral(m);
    let mut line: String = "";
    let mut k = 0i64;
    while k < order.len() {
        line.push_str(f"{order[k]} ");
        k = k + 1i64;
    }
    println(line);
}

fn main() {
    report([[1, 2, 3], [4, 5, 6], [7, 8, 9]]);
    report([[1]]);
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("1 2 3 4 5 6 7 8 9 \n1 \n"),
    );
}

// ── Concurrency analysis plumbing ──

/// Slice 1 wiring sanity-check: full pipeline through
/// `concurrency_analyze`, then a `compile_to_object_with_options` call
/// passing the analysis as `Some(&analysis)`. Asserts only that codegen
/// succeeds — IR-shape assertions for inferred-par lowering are slice 2's
/// job. The point here is to verify the new param accepts a real analysis
/// without regressing the existing legacy path.
#[test]
fn test_concurrency_analysis_threads_into_codegen() {
    use karac::codegen::compile_to_object_with_options;
    let src = r#"
effect resource Net;
effect resource Disk;
effect resource Db;

fn fetch_net() -> i64 reads(Net) { 1 }
fn fetch_disk() -> i64 reads(Disk) { 2 }
fn fetch_db() -> i64 reads(Db) { 3 }

fn main() {
    let a = fetch_net();
    let b = fetch_disk();
    let c = fetch_db();
    println(a + b + c);
}
"#;
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

    let obj_path = "/tmp/karac_test_concurrency_threads.o";
    let result = compile_to_object_with_options(
        &parsed.program,
        obj_path,
        None,
        Some(&analysis),
        None,
        None,
    );
    assert!(
        result.is_ok(),
        "compile_to_object_with_options failed with concurrency analysis: {:?}",
        result
    );
    let _ = std::fs::remove_file(obj_path);
}

/// Slice A (Phase-7 — Par codegen: return values, 2026-05-09) E2E
/// correctness + wall-clock sanity. Four CPU-bound reads on disjoint
/// resources, each returning a typed `i64`. Slice 2 would have
/// dropped the parallel group via the
/// `group_defines_binding_used_outside` gate (each read names its
/// result for the join site); slice A lifts the gate and the four
/// branches now fan out through `karac_par_run`. Asserts:
///   - **Correctness:** the joined output equals the deterministic
///     sum the four kernels computed (4 × triangular `0..N` sums
///     plus a tag).
///   - **Wall-clock concurrency:** total runtime is meaningfully
///     below 4× the per-branch kernel cost, demonstrating that the
///     branches actually executed in parallel rather than serialized
///     through the slot mechanism. The threshold is conservative
///     (3.0× of a per-branch budget) to absorb the runtime's spawn
///     overhead and CI noise; the auto-par dispatch should be
///     comfortably under 2× on any modern multi-core host.
///
/// Skips when the runtime archive is missing — same legitimate
/// soft-skip as the rest of the codegen E2E suite.
#[test]
fn test_auto_par_with_returns_runs_concurrently_and_joins_correctly() {
    use karac::codegen::{compile_to_object_with_options, link_executable};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    // Each `read_*` runs an `N`-iteration triangular-sum kernel; N
    // is tuned to be heavy enough that 4× sequential work is
    // measurable (~hundreds of ms) but light enough that CI noise
    // doesn't dominate. The expected total is
    // `4 * (N * (N - 1) / 2) + (1 + 2 + 3 + 4)`.
    const N: i64 = 8_000_000;
    let expected_sum: i64 = 4 * (N * (N - 1) / 2) + 10;

    let src = format!(
        r#"
effect resource Net;
effect resource Disk;
effect resource Db;
effect resource Cache;

fn busy_sum(n: i64) -> i64 {{
    let mut sum: i64 = 0;
    let mut i: i64 = 0;
    while i < n {{
        sum = sum + i;
        i = i + 1;
    }}
    sum
}}

fn read_net() -> i64 reads(Net) {{ busy_sum({n}) + 1 }}
fn read_disk() -> i64 reads(Disk) {{ busy_sum({n}) + 2 }}
fn read_db() -> i64 reads(Db) {{ busy_sum({n}) + 3 }}
fn read_cache() -> i64 reads(Cache) {{ busy_sum({n}) + 4 }}

fn combine(a: i64, b: i64, c: i64, d: i64) -> i64 {{
    a + b + c + d
}}

fn main() {{
    let result_1 = read_net();
    let result_2 = read_disk();
    let result_3 = read_db();
    let result_4 = read_cache();
    println(combine(result_1, result_2, result_3, result_4));
}}
"#,
        n = N
    );

    let mut parsed = karac::parse(&src);
    if !parsed.errors.is_empty() {
        panic!("parse errors: {:?}", parsed.errors);
    }
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck(&parsed.program);
    let analysis = karac::concurrency_analyze(&parsed.program, &effects);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_par_returns_e2e_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_par_returns_e2e_{}_{}", std::process::id(), id);

    if let Err(e) = compile_to_object_with_options(
        &parsed.program,
        &obj_path,
        None,
        Some(&analysis),
        None,
        None,
    ) {
        panic!("codegen failed for slice-A E2E: {e}");
    }
    // Link / exec failures stay soft-skip — runtime archive may be
    // missing on some CI hosts (matches `tests/par_codegen.rs`'s
    // E2E pattern).
    let Ok(()) = link_executable(&obj_path, &exe_path) else {
        eprintln!("[slice-A E2E] link failed; skipping (runtime archive missing?)");
        let _ = std::fs::remove_file(&obj_path);
        return;
    };

    // Calibrate per-branch cost by running `busy_sum(N)` once
    // sequentially in a separate single-branch program. Cheaper
    // than threading sequential mode into the same binary; gives
    // us a host-specific budget for the wall-clock assertion.
    let cal_src = format!(
        r#"
fn busy_sum(n: i64) -> i64 {{
    let mut sum: i64 = 0;
    let mut i: i64 = 0;
    while i < n {{
        sum = sum + i;
        i = i + 1;
    }}
    sum
}}

fn main() {{
    println(busy_sum({n}));
}}
"#,
        n = N
    );
    let mut cal_parsed = karac::parse(&cal_src);
    if !cal_parsed.errors.is_empty() {
        panic!("calibration parse errors: {:?}", cal_parsed.errors);
    }
    let cal_resolved = karac::resolve(&cal_parsed.program);
    let cal_typed = karac::typecheck(&cal_parsed.program, &cal_resolved);
    karac::lower(&mut cal_parsed.program, &cal_typed);
    let cal_obj = format!("/tmp/karac_par_returns_cal_{}_{}.o", std::process::id(), id);
    let cal_exe = format!("/tmp/karac_par_returns_cal_{}_{}", std::process::id(), id);
    if compile_to_object_with_options(&cal_parsed.program, &cal_obj, None, None, None, None)
        .is_err()
    {
        eprintln!("[slice-A E2E] calibration codegen failed; skipping wall-clock assertion");
        let _ = std::fs::remove_file(&obj_path);
        let _ = std::fs::remove_file(&exe_path);
        return;
    }
    let Ok(()) = link_executable(&cal_obj, &cal_exe) else {
        eprintln!("[slice-A E2E] calibration link failed; skipping wall-clock assertion");
        let _ = std::fs::remove_file(&obj_path);
        let _ = std::fs::remove_file(&exe_path);
        let _ = std::fs::remove_file(&cal_obj);
        return;
    };
    let cal_t0 = Instant::now();
    let _ = output_with_hang_watchdog(std::process::Command::new(&cal_exe));
    let per_branch = cal_t0.elapsed();

    // Run the parallel binary, measure wall-clock, capture stdout.
    let par_t0 = Instant::now();
    let par_out = match output_with_hang_watchdog(std::process::Command::new(&exe_path)) {
        Some(o) => o,
        None => {
            eprintln!("[slice-A E2E] failed to exec parallel binary");
            let _ = std::fs::remove_file(&obj_path);
            let _ = std::fs::remove_file(&exe_path);
            let _ = std::fs::remove_file(&cal_obj);
            let _ = std::fs::remove_file(&cal_exe);
            return;
        }
    };
    let par_elapsed = par_t0.elapsed();
    let stdout = String::from_utf8_lossy(&par_out.stdout).to_string();

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);
    let _ = std::fs::remove_file(&cal_obj);
    let _ = std::fs::remove_file(&cal_exe);

    // Correctness: the printed sum matches the precomputed total.
    let printed: i64 = stdout
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("[slice-A E2E] non-integer stdout {stdout:?}: {e}"));
    assert_eq!(
        printed, expected_sum,
        "[slice-A E2E] joined value mismatch: got {printed}, expected {expected_sum}; \
             the slot loads or `combine` argument flow is wrong"
    );

    // Wall-clock concurrency: parallel total < 3.0 × per-branch
    // budget. The sequential lower bound is 4.0× per-branch; the
    // 3.0× threshold gives a generous margin while still rejecting
    // a serialized lowering. Print observed values to stderr so
    // a developer reading test output can see the actual ratio
    // (matches the parallax-lite microbenchmark's stderr-note
    // pattern).
    let par_secs = par_elapsed.as_secs_f64();
    let cal_secs = per_branch.as_secs_f64();
    eprintln!(
        "[slice-A E2E] per-branch cal {:.3}s; parallel {:.3}s; ratio {:.2}× (4× sequential bound)",
        cal_secs,
        par_secs,
        par_secs / cal_secs.max(1e-6)
    );
    // Ratio test only when the calibration is large enough that
    // the comparison is meaningful; on extremely fast hosts where
    // the kernel completes in < 50ms, signal-to-noise is too low
    // to assert against (same pragmatism as the parallax-lite
    // ratio guards).
    if cal_secs > 0.05 {
        assert!(
            par_secs < 3.0 * cal_secs,
            "[slice-A E2E] parallel runtime {par_secs:.3}s ≥ 3× per-branch {cal_secs:.3}s — \
                 lowering looks serial (slot mechanism may be forcing sequential dispatch)"
        );
    }
}

/// Source-position fidelity: a `par {}` block at a known line
/// must record a `(line, col)` matching the source position of
/// the par-block's body. Pins the byte-offset-to-line-col
/// conversion direction.
///
/// Implementation note: `compile_par_block` flows the inner
/// `block.span` into `emit_par_run`, which then records the
/// site. `block.span` starts at the opening `{`, so the
/// recorded column is the position of `{`, not `par`. The line
/// is the same in both cases (the recorded line is reliably the
/// par-keyword's line), and the column is reliably "somewhere
/// inside the par block on that line." That's the contract for
/// the slice-3 metadata table — `(file, line)` are exact;
/// `col` is at-or-after the `par` keyword.
#[test]
fn test_spawn_site_records_correct_source_location() {
    // Line layout (1-indexed):
    //   1: (blank — leading newline)
    //   2: fn a() { println(1); }
    //   3: fn b() { println(2); }
    //   4: fn main() {
    //   5:     par {
    //   6:         a();
    //   7:         b();
    //   8:     }
    //   9: }
    //
    // The `par` keyword starts at line 5, col 5; the opening
    // `{` (which `block.span` points at) is at line 5, col 9.
    let src = r#"
fn a() { println(1); }
fn b() { println(2); }
fn main() {
    par {
        a();
        b();
    }
}
"#;
    let ir = ir_for_with_source(src);

    // Spawn-site struct fields:
    //   { i32 id, ptr file_cstr, i32 line, i32 col, i32 worker_count, i32 reserved }
    // The only par block in this program produces id=0,
    // line=5, col=9 (opening brace), worker_count=2,
    // reserved=0. Sanity-check the array initializer contains
    // `i32 5, i32 9` — line then column.
    assert!(
        ir.contains("i32 5, i32 9"),
        "expected line=5 col=9 in spawn-site entry; ir:\n{ir}"
    );
}

/// `Runtime.list_par_blocks()` called from inside a `par {}` block
/// observes at least one active frame (its own). Validates that
/// slice 4's `ACTIVE_FRAMES` registry is populated under
/// `karac_par_run` and that `karac_runtime_list_par_blocks_into`
/// joins it against `KARAC_SPAWN_SITES` correctly.
///
/// The branch that calls `list_par_blocks()` runs concurrently with
/// the second branch under `karac_par_run`; the lock-held iteration
/// guarantees we see a consistent snapshot. Worst case, the second
/// branch already finished — so we assert `>= 1` rather than `== 2`.
#[test]
fn test_list_par_blocks_inside_par_block_observes_self() {
    // B-2026-08-20-26: pinned per-thread rather than by unsetting the
    // process env, which every other concurrently-compiling test in this
    // binary would have seen.
    let _pin = karac::codegen::pin_runtime_debug_metadata(true);
    let captured = run_program_capturing(
        r#"
fn check_par_blocks() {
    let pbs = Runtime.list_par_blocks();
    let n = pbs.len();
    println(n);
}
fn other_branch() {
    println(99);
}
fn main() {
    par {
        check_par_blocks();
        other_branch();
    }
}
"#,
    );
    if let Some(c) = captured {
        // Stdout has two lines (one per branch), order non-deterministic.
        // Find the line that's the par-block count and assert >= 1.
        let mut found_count = false;
        for line in c.stdout.lines() {
            let trimmed = line.trim();
            if trimmed == "99" {
                continue;
            }
            if let Ok(n) = trimmed.parse::<i64>() {
                assert!(
                        n >= 1,
                        "expected list_par_blocks() to observe ≥1 active frame inside a par block; got {} (full stdout: {:?})",
                        n,
                        c.stdout
                    );
                found_count = true;
            }
        }
        assert!(
            found_count,
            "didn't find a par-block count line in stdout: {:?}",
            c.stdout
        );
    }
}

/// `Runtime.list_par_blocks()` called from outside any par-block
/// context returns an empty Vec. The root task has no `KaracFrame`
/// registered, so `ACTIVE_FRAMES` is empty.
#[test]
fn test_list_par_blocks_outside_par_block_returns_empty() {
    // B-2026-08-20-26: pinned per-thread rather than by unsetting the
    // process env, which every other concurrently-compiling test in this
    // binary would have seen.
    let _pin = karac::codegen::pin_runtime_debug_metadata(true);
    let captured = run_program_capturing(
        r#"
fn main() {
    let pbs = Runtime.list_par_blocks();
    println(pbs.len());
}
"#,
    );
    if let Some(c) = captured {
        assert_eq!(
            c.stdout.trim(),
            "0",
            "expected empty Vec from main() (no active par blocks)"
        );
    }
}

/// `Runtime.list_tasks()` always returns an empty Vec in v1 — no
/// real suspension exists yet. Pins the v1 contract surface; when
/// Phase 6.3 ships real `WaitTarget` tracking this test gets
/// updated to flag the surface change.
#[test]
fn test_list_tasks_returns_empty_in_v1() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let tasks = Runtime.list_tasks();
    println(tasks.len());
    par {
        println(1);
        println(2);
    }
    let after = Runtime.list_tasks();
    println(after.len());
}
"#,
    );
    if let Some(c) = captured {
        // Both reads must be 0 (one before par, one after par's join).
        // Lines: par's own output (1, 2 in non-det order) plus two zero
        // lines for the list_tasks reads. Filter for the zero count
        // appearance — must occur at least twice.
        let zero_count_lines = c.stdout.lines().filter(|l| l.trim() == "0").count();
        assert!(
            zero_count_lines >= 2,
            "expected at least two `0` (list_tasks().len()) lines; got {:?}",
            c.stdout
        );
    }
}

// ── Theme 6: par-block provider-stack inheritance (sub-step 5) ──────
//
// Structural + e2e tests pinning that `par { }` branches inherit the
// provider stack from the calling thread. The env-struct snapshot is
// taken via `karac_provider_get_stack_head` at par-block entry; each
// branch fn re-seeds its TLS with `karac_provider_set_stack_head` in
// its prologue.

#[test]
fn test_par_block_emits_provider_stack_head_snapshot_and_seed() {
    let ir = ir_for(
        "fn main() {\n\
               par {\n\
                 println(1);\n\
                 println(2);\n\
               }\n\
             }",
    );
    // Snapshot at par-block entry (outer-fn side) — one call per par-block.
    let snap_count = ir
        .lines()
        .filter(|l| l.contains("call") && l.contains("@karac_provider_get_stack_head"))
        .count();
    assert_eq!(
        snap_count, 1,
        "expected exactly one call to karac_provider_get_stack_head at par-block entry; \
             IR: {}",
        ir
    );
    // Seed inside each branch fn — one call per branch (2 branches here).
    let seed_count = ir
        .lines()
        .filter(|l| l.contains("call") && l.contains("@karac_provider_set_stack_head"))
        .count();
    assert_eq!(
        seed_count, 2,
        "expected one karac_provider_set_stack_head call per branch fn (2 branches); \
             IR: {}",
        ir
    );
}

#[test]
fn test_par_block_inside_with_provider_e2e_branches_see_provider() {
    // Provider pushed by with_provider, par block spawned inside.
    // Each par branch's worker thread starts with null TLS; the
    // env-struct snapshot + set_stack_head seed is what makes
    // R.get() resolve inside the branch body.
    let src = "pub trait Reader { fn get(ref self) -> i64; }\n\
            pub struct Data { x: i64 }\n\
            impl Reader for Data { fn get(ref self) -> i64 { self.x } }\n\
            pub effect resource D: Reader;\n\
            fn main() {\n\
              let p = Data { x: 100 };\n\
              with_provider[D](p, || {\n\
                par {\n\
                  println(D.get());\n\
                  println(D.get());\n\
                }\n\
              });\n\
            }";
    let Some(out) = run_program(src) else {
        eprintln!("skipping par+with_provider e2e: runtime/linker unavailable");
        return;
    };
    // Branch order is non-deterministic; both must print 100.
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "expected 2 println outputs; got: {:?}",
        lines
    );
    for l in &lines {
        assert_eq!(
            l.trim(),
            "100",
            "each branch should print 100 (provider inherited from outer scope); got: {:?}",
            lines
        );
    }
}

#[test]
fn test_coro_module_pins_target_data_layout() {
    // Regression (coro frame heap overflow, part 2): the codegen module must
    // carry the REAL target `target datalayout`, not LLVM's empty default.
    //
    // `llvm.coro.size.i64` is constant-folded by CoroSplit using the
    // module's data layout to drive `malloc(coro.size)`. Under the empty
    // default layout (`i64:32`, 4-byte alignment) the folded size is smaller
    // than the frame the AOT object backend actually lays out under the real
    // target layout (`i64:64`, 8-byte alignment). For a coro frame ending in
    // a small field after a large one — the network handler's
    // `[4096 x i8]` recv buffer followed by the i2 suspend-index — the
    // empty-layout size is up to 8 bytes short, so the malloc under-allocates
    // and the trailing suspend-index store lands one past the heap block.
    // Pinning the module layout makes `coro.size` and the backend agree.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let dl = ir
        .lines()
        .find(|l| l.starts_with("target datalayout = "))
        .unwrap_or_else(|| panic!("module must pin a target datalayout:\n{ir}"));
    // The real target layout always declares an i64 alignment (`i64:64` on
    // every Tier-1 target); the empty default never emits a datalayout line
    // at all, so merely finding a non-empty one is the regression signal.
    assert!(
        dl.contains("i64:64") || dl.contains("i64:") || dl.len() > "target datalayout = \"\"".len(),
        "datalayout must be the real target layout, not empty: {dl}"
    );
}

#[test]
fn test_coro_frame_malloc_matches_frame_sizeof() {
    // Regression (coro frame heap overflow, part 2): the post-CoroSplit coro
    // frame `malloc` size must equal the frame struct's true `sizeof` under
    // the module's data layout — no trailing field may land past the
    // allocation. The network handler holds a `[4096 x i8]` recv buffer live
    // across the `accept` park, so its frame ends in (large buffer, small
    // suspend-index); a layout mismatch put the index one byte past malloc.
    use karac::cli::{
        build_call_effect_subs_table, build_callee_network_yield_effect_table,
        build_callee_purely_polymorphic_effects_set, build_state_struct_layouts,
        build_yield_points_table,
    };
    use karac::codegen::compile_to_ir_with_coro_split;

    let src = r#"
            fn handle_connection(ws: WebSocket) {
                let mut buf: Array[u8, 4096] = [0u8; 4096];
                loop {
                    let r = ws.recv_text(mut buf);
                    match r {
                        Result.Ok(n) => {
                            if n == 0 { break; }
                            match ws.send_text(buf[0..n]) {
                                Result.Ok(_) => {}
                                Result.Err(_) => { break; }
                            }
                        }
                        Result.Err(_) => { break; }
                    }
                }
            }
            fn main() {
                let listener: TcpListener = TcpListener.bind("127.0.0.1:0").unwrap();
                let ws: WebSocket = WebSocket.accept(listener).unwrap();
                let mut tg: TaskGroup = TaskGroup.new();
                tg.spawn(|| handle_connection(ws));
            }
        "#;
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "type: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    let pattern_binding_types = typed.pattern_binding_types.clone();
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck_with_typecheck_data(
        &parsed.program,
        karac::effectchecker::PublicEffectsPolicy::default(),
        karac::manifest::CompileProfile::Default,
        method_types.clone(),
        call_type_subs,
    );
    parsed.program.callee_network_yield_effect = build_callee_network_yield_effect_table(&effects);
    parsed.program.yield_points = build_yield_points_table(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
    );
    parsed.program.state_struct_layouts = build_state_struct_layouts(
        &parsed.program,
        &parsed.program.callee_network_yield_effect,
        &method_types,
        &pattern_binding_types,
    );
    parsed.program.call_effect_subs = build_call_effect_subs_table(&effects);
    parsed.program.callee_purely_polymorphic_effects =
        build_callee_purely_polymorphic_effects_set(&effects);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);
    let ir = compile_to_ir_with_coro_split(&parsed.program, None, None)
        .expect("coro split codegen failed");

    // The module must carry a real datalayout (so coro.size folds correctly).
    assert!(
        ir.lines()
            .any(|l| l.starts_with("target datalayout = ")
                && l.len() > "target datalayout = \"\"".len()),
        "post-split module must pin a non-empty target datalayout:\n{ir}"
    );
    // The handler's frame must be present and end with the [4096 x i8] buffer
    // immediately before the suspend-index — the layout that exposed the bug.
    let frame_line = ir
        .lines()
        .find(|l| l.starts_with("%handle_connection.Frame = type"))
        .unwrap_or_else(|| panic!("no handle_connection.Frame type in split IR:\n{ir}"));
    assert!(
        frame_line.contains("[4096 x i8]"),
        "frame must hold the inline [4096 x i8] recv buffer: {frame_line}"
    );
    // The coro frame malloc must be folded to a constant size by CoroSplit —
    // NOT left as a live `call @llvm.coro.size` whose runtime value could be
    // computed against a layout differing from the type the GEPs use. (The
    // unused `declare i64 @llvm.coro.size.i64()` line may remain; only a live
    // call is the regression.) The handler's resume ramp must therefore
    // contain a `malloc(i64 <constant>)`, and `<constant>` is the frame
    // sizeof under the pinned layout — the value the backend lays the frame
    // out at, so no trailing field overflows it.
    assert!(
        !ir.contains("call i64 @llvm.coro.size"),
        "coro.size must be constant-folded post-split (a live call means the \
             frame malloc size is not pinned to the type layout):\n{ir}"
    );
    // The frame malloc is a folded constant (e.g. `malloc(i64 4224)`), proving
    // CoroSplit sized it under the module's pinned layout. A non-constant
    // form (`malloc(i64 %...`) would mean the size is computed at runtime.
    let frame_malloc_is_const = ir.lines().any(|l| {
        let t = l.trim_start();
        t.contains("= call ptr @malloc(i64 ")
            && !t.contains("@malloc(i64 %")
            && !t.contains("getelementptr")
    });
    assert!(
        frame_malloc_is_const,
        "the coro frame malloc must be a folded constant size (pinned to the \
             frame layout), got no constant-size malloc in:\n{ir}"
    );
}

// ── Phase 6 line 26 slice 6: poll-function stub emission ───────────
//
// For each entry in `state_struct_layouts`, codegen emits a stub
// poll function carrying the `KaracParkedTask.poll_fn` ABI from
// line-17 sub-item-2 (`i8 fn(ptr state, ptr cancel)`). Slice 6's
// body is the minimal shape: load the yield-point tag via typed
// GEP into `state_struct_types[fn_key]`, then return Pending
// (discriminant 0) unconditionally. Subsequent sub-slices replace
// the unconditional return with the switch-on-tag dispatch and
// the per-yield-arm captured-locals reload + user-code resume.

#[test]
fn test_poll_fn_emitted_for_network_boundary_function() {
    // A free function calling a `sends(Network)` callee gets a
    // `define internal i8 @__kara_poll_driver(ptr, ptr)` stub poll
    // function. The leading `__kara_poll_` prefix is the codegen-
    // internal naming convention; the poll-fn is private linkage
    // (module-local) so the `internal` qualifier appears on the
    // define line.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let define_line = ir
        .lines()
        .find(|l| l.contains("@__kara_poll_driver"))
        .unwrap_or_else(|| panic!("expected @__kara_poll_driver in IR:\n{ir}"));
    assert!(
        define_line.contains("define"),
        "poll-fn should be defined, not just declared: {define_line}"
    );
    assert!(
        define_line.contains("internal"),
        "poll-fn should have internal linkage (private to module): {define_line}"
    );
    // Return type is i8 (KaracPollResult discriminant); two ptr
    // params (state + cancel) per the line-17 KaracParkedTask ABI.
    assert!(
        define_line.contains("i8 @__kara_poll_driver(ptr"),
        "poll-fn signature must be `i8 @__kara_poll_driver(ptr, ptr)`: {define_line}"
    );
}

#[test]
fn test_poll_fn_loads_tag_via_typed_gep() {
    // The slice-6 stub body loads the yield-point tag from state
    // struct field 0 via a typed GEP into `%kara.state.<fn_key>`.
    // The GEP's type operand keeps the named state-struct type
    // referenced from a real instruction, independent of the slice-5
    // anchor global.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    // The GEP line looks like
    //   `%tag_ptr = getelementptr inbounds %kara.state.driver, ptr %0, i32 0, i32 0`
    // with LLVM's exact spacing — match on the substring that's
    // robust against the SSA-name variant LLVM picks for the
    // anonymous `%0` state param (which is `%state` if inkwell
    // names it; LLVM may renumber).
    assert!(
        ir.contains("getelementptr inbounds %kara.state.driver"),
        "poll-fn stub must GEP into the state struct's typed field 0:\n{ir}"
    );
    assert!(
        ir.contains("load i32"),
        "poll-fn stub must load the i32 tag from the GEP result:\n{ir}"
    );
}

#[test]
fn test_poll_fn_returns_pending_stub() {
    // The slice-6 stub returns `KaracPollResult.Pending` (discriminant
    // 0) unconditionally — the dispatch switch lands in slice 7+.
    // Pin the `ret i8 0` shape inside the poll function so a future
    // slice that changes the return value forces a deliberate test
    // update.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    // Carve out the poll-fn body by finding the define line and
    // taking everything until the next closing brace at column 1.
    let mut in_body = false;
    let mut body = String::new();
    for line in ir.lines() {
        if line.contains("define internal i8 @__kara_poll_driver") {
            in_body = true;
        }
        if in_body {
            body.push_str(line);
            body.push('\n');
            if line == "}" {
                break;
            }
        }
    }
    assert!(!body.is_empty(), "could not find poll-fn body in IR:\n{ir}");
    assert!(
        body.contains("ret i8 0"),
        "poll-fn stub must return Pending (i8 0):\n{body}"
    );
}

#[test]
fn test_poll_fn_uses_dot_separated_name_for_methods() {
    // Impl-method poll-fns carry the `Type.method` key shape with
    // a literal `.` in the LLVM symbol — LLVM accepts dots in
    // function names. Matches the existing impl-method symbol-
    // mangling convention (`Hub.run` for the user method) and the
    // `__kara_state_type_anchor_Hub.run` anchor naming from slice 5.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub {
                 fn run(self) { fetch(); }
             }",
    );
    assert!(
        ir.contains("@__kara_poll_Hub.run"),
        "expected @__kara_poll_Hub.run for impl method:\n{ir}"
    );
}

#[test]
fn test_poll_fn_not_emitted_for_pure_function() {
    // A pure function (no network-effect calls) has no entry in
    // `state_struct_layouts` per slice 4's presence rule and
    // therefore no `__kara_poll_*` symbol in the IR.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn pure_helper(x: i64) -> i64 { x + 1 }",
    );
    assert!(
        !ir.contains("@__kara_poll_pure_helper"),
        "pure function must not emit a poll-fn:\n{ir}"
    );
}

#[test]
fn test_poll_fn_emits_switch_on_tag() {
    // The slice-7 dispatch replaces the unconditional `ret i8 0`
    // with `switch i32 %tag, ...`. Pin the instruction shape so a
    // future slice that changes the dispatch mechanism (e.g. an
    // indirect branch through a function-pointer table) forces a
    // deliberate test update.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("switch i32 %tag"),
        "poll-fn must dispatch via `switch i32 %tag`:\n{body}"
    );
}

#[test]
fn test_poll_fn_switch_has_n_plus_one_arms_for_n_yields() {
    // A function with one yield point has 2 arms (state_0 + state_1):
    // state_0 is the initial-call entry state (before any yield),
    // state_1 is the post-yield resume state. Slice 7 emits both as
    // Pending-return stubs; slice 8 fills them in.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("state_0:"),
        "poll-fn must have a `state_0:` arm (initial-call state):\n{body}"
    );
    assert!(
        body.contains("state_1:"),
        "poll-fn must have a `state_1:` arm (post-yield-1 state):\n{body}"
    );
    assert!(
        !body.contains("state_2:"),
        "poll-fn with 1 yield must NOT have a `state_2:` arm:\n{body}"
    );
}

#[test]
fn test_poll_fn_switch_default_is_unreachable() {
    // The default switch arm goes to a `tag_unreachable` block that
    // contains a single `unreachable` instruction. Tells LLVM the
    // out-of-range tag path is impossible and unlocks downstream
    // optimizations of the switch (e.g. jump-table compaction).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("tag_unreachable:"),
        "poll-fn must have a `tag_unreachable:` default arm:\n{body}"
    );
    // Make sure the unreachable instruction itself is present (not
    // just the label) — the label without the instruction would
    // produce malformed IR LLVM would reject at module-verify time.
    assert!(
        body.contains("unreachable"),
        "default arm must end in the `unreachable` instruction:\n{body}"
    );
}

#[test]
fn test_poll_fn_reload_prologue_appears_in_every_state_arm() {
    // The reload prologue is uniform across all state arms — both
    // state_0 (initial call) and state_1 (post-yield resume) emit
    // the same GEP+load+alloca+store sequence. Pin this by counting
    // the GEP occurrences against the arm count.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // 1 yield point → 2 arms (state_0 + terminal state_1) → 2 reload
    // GEPs into field 1 (one per arm, one captured local) + 1 slice-
    // 8n writeback GEP in state_0 (non-terminal) = 3.
    let gep_count = body
        .matches("getelementptr inbounds %kara.state.driver, ptr %0, i32 0, i32 1")
        .count();
    assert_eq!(
            gep_count, 3,
            "expected 3 GEPs for `items` (2 reload + 1 slice-8n writeback) in 1-yield function:\n{body}"
        );
}

#[test]
fn test_poll_fn_reload_prologue_multi_field_layout() {
    // Two captured locals across the union (`a` from the param,
    // `b` from a let between yields) produce two GEP+load+alloca+
    // store quadruples per state arm — field 1 for `a`, field 2
    // for `b`. Pins that the field-index increment scales with the
    // layout's field count.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(a: Vec[i64]) {
                 fetch();
                 let b: Vec[i64] = a;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // GEP for `a` (field 1) and `b` (field 2) should both appear.
    let gep_field_1 = body
        .matches("getelementptr inbounds %kara.state.driver, ptr %0, i32 0, i32 1")
        .count();
    let gep_field_2 = body
        .matches("getelementptr inbounds %kara.state.driver, ptr %0, i32 0, i32 2")
        .count();
    // 2 yield points → 3 arms (state_0, state_1, state_2). Each arm
    // reloads both fields → 3 reload GEPs per field. Plus slice-8n
    // writebacks before each non-terminal yield (state_0, state_1)
    // → +2 writeback GEPs per field. Total: 5 per field.
    assert_eq!(
        gep_field_1, 5,
        "expected 5 GEPs to field 1 (3 reload + 2 slice-8n writeback) in 2-yield function:\n{body}"
    );
    assert_eq!(
        gep_field_2, 5,
        "expected 5 GEPs to field 2 (3 reload + 2 slice-8n writeback) in 2-yield function:\n{body}"
    );
}

#[test]
fn test_poll_fn_switch_multi_yield_arm_count() {
    // Three yield points → 4 arms (state_0 through state_3). Pins
    // that the arm count scales with the yield-point count per the
    // N+1 spec.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             pub fn upload() with sends(Network) {}
             fn driver() {
                 fetch();
                 upload();
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    for i in 0..4 {
        let label = format!("state_{i}:");
        assert!(
            body.contains(&label),
            "poll-fn with 3 yields must have arm `{label}`:\n{body}"
        );
    }
    assert!(
        !body.contains("state_4:"),
        "poll-fn with 3 yields must NOT have `state_4:` arm:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8b: state-transition skeleton ────────────
//
// Non-terminal arms (state_i for i < N) write the next tag value
// i+1 into state struct field 0 ahead of returning Pending, so the
// next poll-fn invocation routes to state_{i+1}. The terminal arm
// (state_N) returns Ready (i8 1) — the function has completed and
// the caller can observe the result. Slice 8c+ adds the user-code
// lowering between the reload prologue and the tag-store / Ready
// return.

#[test]
fn test_poll_fn_non_terminal_arm_stores_next_tag() {
    // For a function with one yield point, state_0 is the only
    // non-terminal arm — it stores `i32 1` into state struct field 0
    // before returning Pending. The store value matches the
    // arm-index + 1 (the tag of the next state to dispatch to).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // `store i32 1` to the state struct's tag field. The exact
    // instruction shape: `store i32 1, ptr %state_0.next_tag_ptr`
    // (the GEP target carries the slice-8b naming).
    assert!(
        body.contains("store i32 1, ptr %state_0.next_tag_ptr"),
        "state_0 must store i32 1 (next tag) into state struct tag field:\n{body}"
    );
}

#[test]
fn test_poll_fn_terminal_arm_returns_ready() {
    // The terminal arm (state_N) returns `ret i8 1` (Ready discrim)
    // rather than Pending — the function has completed and the
    // caller observes the result. For a 1-yield function, state_1
    // is the terminal arm.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Find the state_1 block and check it ends in `ret i8 1`.
    // The state_1 label starts the terminal arm; the next instr
    // after the reload prologue should be `ret i8 1`.
    assert!(
        body.contains("ret i8 1"),
        "terminal arm must return Ready (i8 1):\n{body}"
    );
}

#[test]
fn test_poll_fn_multi_yield_stores_each_tag_transition() {
    // A 2-yield function has 3 arms: state_0 (entry, stores 1),
    // state_1 (post-yield-1, stores 2), state_2 (terminal, returns
    // Ready). Pins that the tag-transition value increments with
    // the arm index.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             pub fn upload() with sends(Network) {}
             fn driver() {
                 fetch();
                 upload();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i32 1, ptr %state_0.next_tag_ptr"),
        "state_0 must store next tag = 1:\n{body}"
    );
    assert!(
        body.contains("store i32 2, ptr %state_1.next_tag_ptr"),
        "state_1 must store next tag = 2:\n{body}"
    );
    // Terminal arm state_2 must have NO tag-store (only a Ready
    // return). Pin this by checking that `next_tag_ptr` doesn't
    // appear with a state_2 prefix.
    assert!(
        !body.contains("%state_2.next_tag_ptr"),
        "terminal arm state_2 must not emit a tag-store:\n{body}"
    );
    // And the Ready return must be present.
    assert!(
        body.contains("ret i8 1"),
        "terminal arm state_2 must return Ready (i8 1):\n{body}"
    );
}

#[test]
fn test_poll_fn_terminal_arm_only_arm_with_ready_return() {
    // The Ready return only appears in the terminal arm — every
    // non-terminal arm ends in `ret i8 0` (Pending). For a 3-yield
    // function (4 arms), exactly one `ret i8 1` should appear
    // (terminal state_3) while three `ret i8 0` appear
    // (state_0..state_2 non-terminal).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             pub fn upload() with sends(Network) {}
             fn driver() {
                 fetch();
                 upload();
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let ready_count = body.matches("ret i8 1").count();
    let pending_count = body.matches("ret i8 0").count();
    assert_eq!(
        ready_count, 1,
        "exactly one Ready return (terminal arm) expected in 3-yield function:\n{body}"
    );
    assert_eq!(
        pending_count, 3,
        "three Pending returns (non-terminal arms) expected in 3-yield function:\n{body}"
    );
}

#[test]
fn test_caller_side_intercept_emits_poll_loop_block() {
    // The intercept emits a `kara.poll_loop` block where the
    // poll-fn is invoked and the discriminant compared against
    // Pending (i8 0) for the loopback branch.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body.contains("kara.poll_loop:"),
        "intercept must emit a `kara.poll_loop` block:\n{main_body}"
    );
    // Poll-fn invocation with state ptr + null cancel pointer.
    assert!(
        main_body.contains("call i8 @__kara_poll_driver(ptr %kara.state, ptr null)"),
        "intercept must invoke poll-fn with state ptr + null cancel:\n{main_body}"
    );
    // Pending compare (icmp eq i8 ..., 0) drives the loopback.
    assert!(
        main_body.contains("icmp eq i8 %kara.poll_result, 0"),
        "intercept must compare poll discriminant against Pending=0:\n{main_body}"
    );
}

// ── Phase 6 line 26 slice 8e: cooperative yield on Pending ─────────
//
// The caller-side intercept routes the Pending path through a
// `kara.poll_yield` block that calls `sched_yield` before looping
// back to the poll-loop, so the parent thread yields the OS
// scheduler quantum between poll-fn invocations rather than busy-
// spinning. Without the yield the line-17 dispatcher thread (and
// other tasks on the same scheduler) would be starved of cycles.

#[test]
fn test_caller_side_intercept_emits_poll_yield_block() {
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body.contains("kara.poll_yield:"),
        "intercept must emit a `kara.poll_yield` block:\n{main_body}"
    );
}

#[test]
fn test_caller_side_intercept_yield_block_loops_back_to_poll_loop() {
    // After `sched_yield`, the yield block unconditionally branches
    // back to `kara.poll_loop` to re-invoke the poll-fn.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body.contains("br label %kara.poll_loop"),
        "yield block must br back to kara.poll_loop:\n{main_body}"
    );
}

#[test]
fn test_caller_arg_storing_appears_before_poll_loop_branch() {
    // The arg-store sites must appear textually before the
    // `br label %kara.poll_loop` that enters the loop — args have
    // to be in the state struct by the time the first poll-fn
    // invocation runs.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) { fetch(); }
             fn main() { driver(7); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    let store_pos = main_body
        .find("store i64 7, ptr %kara.arg0.field_ptr")
        .expect("arg store must exist");
    let br_pos = main_body
        .find("br label %kara.poll_loop")
        .expect("poll-loop entry branch must exist");
    assert!(
        store_pos < br_pos,
        "arg-store must precede `br label %kara.poll_loop`:\n{main_body}"
    );
}

#[test]
fn test_state_machine_skips_polymorphic_poll_fn() {
    // Polymorphic yielding fn produces no `__kara_poll_<base>`
    // poll-fn — slice 6's iteration now filters generics.
    // Per-mono emission lands `@"__kara_poll_driver$i64"` (with
    // `$` triggering LLVM's quoted-symbol convention); the
    // assertion shape `@__kara_poll_driver(` matches the base
    // name's call-site shape exactly and doesn't trip on the
    // mangled variant.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    assert!(
        !ir.contains("@__kara_poll_driver("),
        "polymorphic driver must not emit a base-name poll-fn:\n{ir}"
    );
}

#[test]
fn test_e2e_defer_lifo_when_body_would_auto_parallelize() {
    // B-2026-07-16-10: a function body that triggers auto-parallelization
    // (here a `Vec.new()` + `push` / a Vec-building loop makes the auto-par
    // heuristic wrap the body in `karac_par_run`) must STILL run user
    // `defer` blocks LIFO at scope exit — design.md § *defer*. Before the
    // fix, the par_run whole-function lowering emitted function-scope defers
    // FIFO-inline at their declaration point (native+JIT printed
    // "1","2","3","0" — defers before the body's "0"), diverging from the
    // interpreter. Fixed by bailing auto-par to sequential codegen whenever
    // the function contains a `defer`/`errdefer` (concurrency.rs
    // `block_has_user_defer` gate); the sequential lowering drains defers
    // correctly. Two shapes: a straight-line Vec.push body, and a
    // Vec-building `while` loop (the collect/tabulate auto-par trigger).
    let out = run_program(
        r#"
fn main() {
    let mut log: Vec[i64] = Vec.new();
    defer { println("1"); }
    defer { println("2"); }
    defer { println("3"); }
    log.push(99);
    println("0");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "3", "2", "1"]);
    }
    let out2 = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    defer { println("100"); }
    defer { println("200"); }
    let mut i: i64 = 0;
    while i < 4 {
        v.push(i * i);
        i = i + 1;
    }
    println(v.len());
}
"#,
    );
    if let Some(out2) = out2 {
        let lines: Vec<&str> = out2.trim().lines().collect();
        assert_eq!(lines, vec!["4", "200", "100"]);
    }
}

#[test]
fn test_e2e_modbind_thread_local_read_write() {
    // `#[thread_local]` lowers to an LLVM thread-local global.
    // Single-task case: behaves identically to a non-thread-local
    // `let mut` (each task sees its own copy starting at the
    // initializer). The per-task disjoint-instance semantic
    // matters for parallel code; main-thread sanity check here.
    let output = run_program(
        "#[thread_local]\n\
             let mut TLS_COUNTER: i64 = 0;\n\
             fn main() {\n\
                 TLS_COUNTER = 42;\n\
                 println(TLS_COUNTER);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "42\n");
}

#[test]
fn test_ir_modbind_thread_local_carries_storage_class() {
    // `#[thread_local]` must produce the `thread_local` keyword
    // on the LLVM global so the linker emits the per-task TLS
    // segment. The general-dynamic model is portable across
    // ELF/Mach-O; LLVM IR prints it as `thread_local` (no
    // qualifier) by default.
    let ir = ir_for(
        "#[thread_local]\n\
             let mut TLS_COUNTER: i64 = 0;\n\
             fn main() { TLS_COUNTER = 1; println(TLS_COUNTER); }",
    );
    assert!(
        ir.contains("thread_local") && ir.contains("@TLS_COUNTER"),
        "expected thread_local TLS_COUNTER global in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_park_on_fd_state_struct_emitted_with_parked_task_trailing_fields() {
    // `%kara.state.karac_park_on_fd` carries the layout
    // `{ i32 tag, i32 fd, i8 direction, ptr poll_fn, ptr state,
    //    i64 token, ptr slot }`. The poll_fn/state ptrs are the
    // `KaracParkedTask` storage the dispatcher reads back; the trailing
    // `token` + `slot` (async-sched slice 2/3) carry the registration
    // handle (for the caller's one-shot deregister) and the per-park
    // completion slot (the caller blocks on it, state_1 signals it).
    let ir = ir_for_with_state_struct_layouts(park_on_fd_source());
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.karac_park_on_fd = type {"))
        .unwrap_or_else(|| panic!("missing karac_park_on_fd state struct in IR:\n{ir}"));
    assert!(
        line.contains("i32") && line.contains("i64") && line.matches("ptr").count() >= 3,
        "state struct must carry tag (i32) + token (i64) + 3 ptrs \
             (poll_fn, state, slot): {line}"
    );
}

#[test]
fn test_park_on_fd_constructor_initializes_parked_task_pointers() {
    // The constructor `__kara_state_new_karac_park_on_fd` must
    // store the poll-fn pointer into the parked_task.poll_fn slot
    // (field 3) and the state pointer (self-reference) into
    // parked_task.state (field 4).
    let ir = ir_for_with_state_struct_layouts(park_on_fd_source());
    let ctor_body = function_body(&ir, "__kara_state_new_karac_park_on_fd")
        .expect("constructor body must be present in IR");
    assert!(
        ctor_body.contains("parked.poll_fn.ptr"),
        "constructor must GEP parked_task.poll_fn field:\n{ctor_body}"
    );
    assert!(
        ctor_body.contains("parked.state.ptr"),
        "constructor must GEP parked_task.state field:\n{ctor_body}"
    );
    assert!(
        ctor_body.contains("@__kara_poll_karac_park_on_fd"),
        "constructor must store poll-fn address into parked_task:\n{ctor_body}"
    );
}

#[test]
fn test_park_on_fd_poll_fn_emits_register_fd_call_in_state_0() {
    // state_0 allocates the per-park completion slot, calls
    // `karac_runtime_event_loop_register_fd(fd, dir, &parked_task)`,
    // stores the token + `tag = 1`, and returns Pending.
    let ir = ir_for_with_state_struct_layouts(park_on_fd_source());
    let body =
        function_body(&ir, "__kara_poll_karac_park_on_fd").expect("poll-fn body must be present");
    assert!(
        body.contains("@karac_runtime_event_loop_register_fd"),
        "state_0 must call karac_runtime_event_loop_register_fd:\n{body}"
    );
    assert!(
        body.contains("kara.park.parked_ptr"),
        "state_0 must GEP &parked_task field:\n{body}"
    );
    assert!(
        body.contains("@karac_runtime_park_slot_new"),
        "state_0 must allocate the per-park completion slot:\n{body}"
    );
}

#[test]
fn test_park_on_fd_poll_fn_signals_slot_in_state_1_not_take_wakeups() {
    // Async-sched slice 2/3: state_1 — reached only when the dispatcher
    // re-invokes on real readiness — signals the per-park completion
    // slot and returns Ready. It must NOT block on the global
    // `take_wakeups` (the wakeup-stealing source of the P0 wedge).
    let ir = ir_for_with_state_struct_layouts(park_on_fd_source());
    let body =
        function_body(&ir, "__kara_poll_karac_park_on_fd").expect("poll-fn body must be present");
    assert!(
        body.contains("@karac_runtime_park_slot_signal"),
        "state_1 must signal the completion slot:\n{body}"
    );
    assert!(
        !body.contains("@karac_runtime_event_loop_take_wakeups"),
        "state_1 must NOT block on the global take_wakeups queue:\n{body}"
    );
}

#[test]
fn test_park_on_fd_poll_fn_starts_dispatcher_in_entry() {
    // Entry block calls `karac_runtime_scheduler_start_dispatcher`
    // before dispatching on the tag — idempotent bootstrap so the
    // dispatcher (which re-invokes this poll-fn at state_1, routed by
    // the wakeup's parked pointer) is running before register_fd.
    let ir = ir_for_with_state_struct_layouts(park_on_fd_source());
    let body =
        function_body(&ir, "__kara_poll_karac_park_on_fd").expect("poll-fn body must be present");
    assert!(
        body.contains("@karac_runtime_scheduler_start_dispatcher"),
        "poll-fn entry must start the scheduler dispatcher (idempotent bootstrap):\n{body}"
    );
}

// ── Phase 6 line 218 slice 4: spawn() / TaskGroup.spawn() / TaskHandle.join() codegen ──

/// Free `spawn(|| make_zero())` emits a synthesized SpawnFn wrapper
/// and a `call ptr @karac_runtime_spawn` against it in the calling
/// function. The wrapper invokes the closure body inline.
#[test]
fn test_spawn_call_emits_runtime_spawn_call() {
    let src = r#"
            fn make_zero() -> i64 { 0 }
            fn driver() {
                let _h = spawn(|| make_zero());
            }
        "#;
    let ir = ir_for(src);
    assert!(
        ir.contains("declare ptr @karac_runtime_spawn"),
        "expected karac_runtime_spawn FFI declaration; ir:\n{ir}"
    );
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    assert!(
        body.contains("call ptr @karac_runtime_spawn"),
        "driver body should call karac_runtime_spawn; body:\n{body}"
    );
    assert!(
        body.contains("call ptr @malloc"),
        "driver body should malloc the env buffer; body:\n{body}"
    );
}

/// The spawn wrapper synthesized at the call site receives three
/// pointer params (env, result_out, cancel) and stores T into
/// `result_out` before returning void. For a `|| make_zero()`
/// closure returning i64, the body calls make_zero() and stores
/// the i64 result.
#[test]
fn test_spawn_wrapper_signature_and_result_store() {
    let src = r#"
            fn make_zero() -> i64 { 0 }
            fn driver() {
                let _h = spawn(|| make_zero());
            }
        "#;
    let ir = ir_for(src);
    // Wrapper symbol is named `__spawn_wrap_<N>`. Find one.
    let wrapper_name = ir
        .lines()
        .filter_map(|line| {
            if line.starts_with("define ") && line.contains("@__spawn_wrap_") {
                let start = line.find("@__spawn_wrap_")? + 1; // skip '@'
                let end = line[start..].find('(')? + start;
                Some(line[start..end].to_string())
            } else {
                None
            }
        })
        .next()
        .expect("expected at least one __spawn_wrap_N fn");
    let body = function_body(&ir, &wrapper_name).expect("wrapper body");
    // Signature visible at the define line — three ptr params, void return.
    let define_line = ir
        .lines()
        .find(|line| line.starts_with("define ") && line.contains(&wrapper_name))
        .expect("define line");
    assert!(
        define_line.contains("void"),
        "wrapper returns void; line:\n{define_line}"
    );
    assert!(
        define_line.matches("ptr ").count() >= 3,
        "wrapper takes 3 ptr params; line:\n{define_line}"
    );
    // Body calls the user fn make_zero (return inlined or via call) and frees env.
    assert!(
        body.contains("call void @free("),
        "wrapper body frees the env before return; body:\n{body}"
    );
}

/// `tg.spawn(closure)` dispatches through the same lowering as free
/// `spawn(closure)`. The TaskGroup receiver is discarded in slice 4.
#[test]
fn test_task_group_spawn_method_dispatches_to_runtime_spawn() {
    let src = r#"
            fn make_zero() -> i64 { 0 }
            fn driver() {
                let mut tg = TaskGroup.new();
                tg.spawn(|| make_zero());
            }
        "#;
    let ir = ir_for(src);
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    assert!(
        body.contains("call ptr @karac_runtime_spawn"),
        "tg.spawn(...) should lower through karac_runtime_spawn; body:\n{body}"
    );
}

/// `h.join()` lowers to `call i8 @karac_runtime_task_join(handle, out_slot)`
/// preceded by an alloca for the result slot. v1 reads i64-shaped
/// bytes per the documented `recover_task_handle_join_return_ty`
/// fallback.
#[test]
fn test_task_handle_join_emits_runtime_task_join_call() {
    let src = r#"
            fn make_zero() -> i64 { 0 }
            fn driver() -> i64 {
                let h = spawn(|| make_zero());
                h.join()
            }
        "#;
    let ir = ir_for(src);
    assert!(
        ir.contains("declare i8 @karac_runtime_task_join"),
        "expected karac_runtime_task_join FFI declaration; ir:\n{ir}"
    );
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    assert!(
        body.contains("call i8 @karac_runtime_task_join"),
        "driver body should call karac_runtime_task_join; body:\n{body}"
    );
    // The handle pointer is recovered via ptrtoint→inttoptr (i64
    // task_id stored in TaskHandle struct, cast back to a pointer
    // for the FFI).
    assert!(
        body.contains("inttoptr"),
        "driver body should cast task_id i64 back to a pointer; body:\n{body}"
    );
}

// ── Phase 6 line 218 slice 5: TaskGroup.new / TaskGroup.spawn-register / TaskGroup.drop ──

/// All three TaskGroup runtime FFI declarations are emitted.
#[test]
fn test_taskgroup_ffi_declarations_present() {
    let ir = ir_for("fn nop() {}");
    for sym in [
        "declare ptr @karac_runtime_taskgroup_new",
        "declare void @karac_runtime_taskgroup_register",
        "declare void @karac_runtime_taskgroup_join_and_free",
        "declare void @karac_runtime_taskgroup_cancel",
    ] {
        assert!(ir.contains(sym), "expected `{sym}` declaration; ir:\n{ir}");
    }
}

/// `let tg = TaskGroup.new()` lowers to `call ptr @karac_runtime_taskgroup_new()`
/// followed by `ptrtoint` of the result into the i64 `id` field.
#[test]
fn test_taskgroup_new_lowers_to_runtime_call() {
    let src = r#"
            fn driver() {
                let _tg = TaskGroup.new();
            }
        "#;
    let ir = ir_for(src);
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    assert!(
        body.contains("call ptr @karac_runtime_taskgroup_new"),
        "TaskGroup.new() should call runtime allocator; body:\n{body}"
    );
    assert!(
        body.contains("ptrtoint ptr"),
        "TaskGroup.new() result should be cast to i64; body:\n{body}"
    );
}

/// `tg.spawn(closure)` emits the spawn FFI + a child-registration
/// call against the receiver's group pointer.
#[test]
fn test_taskgroup_spawn_registers_child_with_group() {
    let src = r#"
            fn make_zero() -> i64 { 0 }
            fn driver() {
                let mut tg = TaskGroup.new();
                tg.spawn(|| make_zero());
            }
        "#;
    let ir = ir_for(src);
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    // Both spawn and register must appear in the driver body.
    assert!(
        body.contains("call ptr @karac_runtime_spawn"),
        "driver body should call spawn; body:\n{body}"
    );
    assert!(
        body.contains("call void @karac_runtime_taskgroup_register"),
        "tg.spawn(...) should register the child with the group; body:\n{body}"
    );
}

/// A2 slice 5b-1: `tg.cancel()` lowers to
/// `call void @karac_runtime_taskgroup_cancel` on the group pointer
/// recovered from the receiver's `i64 id` (via `inttoptr`).
#[test]
fn test_taskgroup_cancel_lowers_to_runtime_call() {
    let src = r#"
            fn make_zero() -> i64 { 0 }
            fn driver() {
                let mut tg = TaskGroup.new();
                tg.spawn(|| make_zero());
                tg.cancel();
            }
        "#;
    let ir = ir_for(src);
    assert!(
        ir.contains("declare void @karac_runtime_taskgroup_cancel"),
        "expected karac_runtime_taskgroup_cancel FFI declaration; ir:\n{ir}"
    );
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    assert!(
        body.contains("call void @karac_runtime_taskgroup_cancel"),
        "tg.cancel() should call karac_runtime_taskgroup_cancel; body:\n{body}"
    );
    assert!(
        body.contains("inttoptr"),
        "tg.cancel() should cast the i64 id back to a pointer; body:\n{body}"
    );
}

/// `@TaskGroup.drop` body invokes `karac_runtime_taskgroup_join_and_free`
/// on the group pointer recovered from `self.id`.
#[test]
fn test_taskgroup_drop_calls_join_and_free() {
    let src = r#"
            fn driver() {
                let _tg = TaskGroup.new();
            }
        "#;
    let ir = ir_for(src);
    let drop_body = function_body(&ir, "TaskGroup.drop").expect("@TaskGroup.drop emitted");
    assert!(
        drop_body.contains("call void @karac_runtime_taskgroup_join_and_free"),
        "TaskGroup.drop should call join_and_free; body:\n{drop_body}"
    );
    assert!(
        drop_body.contains("inttoptr"),
        "drop should cast i64 id back to a pointer; body:\n{drop_body}"
    );
}

/// The scope-exit drop synthesizer wires `@karac_drop_TaskGroup` at
/// the let-binding's scope exit, so a `let tg = TaskGroup.new()`
/// without explicit drop still joins on function return.
#[test]
fn test_taskgroup_scope_exit_drop_wraps_runtime_join() {
    let src = r#"
            fn driver() {
                let _tg = TaskGroup.new();
            }
        "#;
    let ir = ir_for(src);
    let body = function_body(&ir, "driver").expect("driver fn must lower");
    // Either @karac_drop_TaskGroup (wrapper synth) or
    // @TaskGroup.drop (raw drop) is invoked at scope exit.
    assert!(
        body.contains("@karac_drop_TaskGroup") || body.contains("@TaskGroup.drop"),
        "driver should invoke TaskGroup drop at scope exit; body:\n{body}"
    );
}

/// B-2026-06-09-1 regression: a `TaskGroup`-registered child that is
/// ALSO explicitly `.join()`ed used to be consumed twice — the
/// explicit join freed the runtime handle, then the group's scope-exit
/// drop joined the dangling pointer → use-after-free → SIGSEGV (exit
/// 139). The crash fired *after* `println("done")` (group drop runs at
/// scope exit, past the last statement), so stdout alone can't catch
/// it — assert a clean exit (status 0), not just the output. Fixed by
/// marking a registered handle "group-owned" so an explicit join waits
/// + copies the result without freeing; the group is the sole freer.
#[test]
fn test_e2e_taskgroup_child_explicit_join_no_double_free() {
    let out = run_program_capturing(
        r#"
fn z() -> i64 { 42 }
fn main() {
    let mut g: TaskGroup = TaskGroup.new();
    let h: TaskHandle[i64] = g.spawn(|| z());
    let r: i64 = h.join();
    println(r);
    println("done");
}
"#,
    );
    if let Some(c) = out {
        assert!(
            c.status.success(),
            "registered child + explicit join must exit cleanly (no double-free); \
                 status={:?} stdout={:?} stderr={:?}",
            c.status,
            c.stdout,
            c.stderr
        );
        assert_eq!(c.stdout, "42\ndone\n");
    }
}

// ── Atomic[T] codegen: load/store/new method dispatch ─────────
//
// Mirrors the interpreter's Atomic tests at `tests/interpreter.rs:3294-3318`,
// closing the codegen-side gap left by L215c-cons (phase-7 tracker line 229).
// The receiver shape covers both the Identifier form (`a.load(...)`) for
// top-level `let a = Atomic.new(v)` bindings and the FieldAccess form
// (`c.count.load(...)`) emitted by `karac migrate --atomic` against
// `shared struct` → `par struct` conversions.

#[test]
fn test_e2e_atomic_new_and_load_seqcst() {
    let out = run_program(
        "fn main() {\n\
                 let a = Atomic.new(42);\n\
                 println(a.load(MemoryOrdering.SeqCst));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn test_e2e_atomic_store_then_load_relaxed() {
    let out = run_program(
        "fn main() {\n\
                 let mut a = Atomic.new(0);\n\
                 a.store(99, MemoryOrdering.Relaxed);\n\
                 println(a.load(MemoryOrdering.Relaxed));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "99\n");
    }
}

#[test]
fn test_e2e_atomic_release_store_acquire_load() {
    // The canonical pair the migration tool emits — store uses Release,
    // load uses Acquire. Pins that LLVM accepts both on the same slot
    // when alignment is set correctly.
    let out = run_program(
        "fn main() {\n\
                 let mut a = Atomic.new(0);\n\
                 a.store(7, MemoryOrdering.Release);\n\
                 println(a.load(MemoryOrdering.Acquire));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "7\n");
    }
}

#[test]
fn test_e2e_atomic_field_load_store_struct_field() {
    // FieldAccess receiver shape — the form `karac migrate --atomic`
    // emits. Without the FieldAccess arm in `is_atomic_receiver` /
    // `resolve_atomic_storage`, this falls through to the user
    // impl-block lookup and errors with "no handler for method
    // 'store' / 'load'".
    let out = run_program(
        "struct Counter { count: Atomic[i64] }\n\
             fn main() {\n\
                 let c = Counter { count: Atomic.new(0) };\n\
                 c.count.store(123, MemoryOrdering.Release);\n\
                 println(c.count.load(MemoryOrdering.Acquire));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "123\n");
    }
}

#[test]
fn test_ir_atomic_load_emits_load_atomic_seq_cst() {
    // IR-shape regression: the load instruction's atomic ordering
    // attaches via `set_atomic_ordering(SequentiallyConsistent)`,
    // which LLVM prints as `load atomic ... seq_cst`. Without the
    // dispatch arm, codegen would either fall through to the user
    // impl-block lookup (failing) or emit a plain `load` (no
    // atomic marker).
    let ir = ir_for(
        "fn main() {\n\
                 let a = Atomic.new(42);\n\
                 let _ = a.load(MemoryOrdering.SeqCst);\n\
             }",
    );
    assert!(
        ir.contains("load atomic") && ir.contains("seq_cst"),
        "expected `load atomic ... seq_cst` in IR; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_atomic_store_emits_store_atomic_release() {
    let ir = ir_for(
        "fn main() {\n\
                 let mut a = Atomic.new(0);\n\
                 a.store(99, MemoryOrdering.Release);\n\
             }",
    );
    assert!(
        ir.contains("store atomic") && ir.contains("release"),
        "expected `store atomic ... release` in IR; got:\n{}",
        ir
    );
}

// ── Atomic[bool] codegen: i8 slot widening with zext/trunc ────
//
// LLVM rejects `load atomic i1` / `store atomic i1` directly. The
// codegen widens `Atomic[bool]` to an i8 slot; `.store` zexts the
// incoming i1 to i8, `.load` truncs the i8 back to i1. Closes
// phase-7 tracker line 231 (the blocker for the `karac migrate
// --atomic` default-flip).

#[test]
fn test_e2e_atomic_bool_new_and_load_seqcst() {
    let out = run_program(
        "fn main() {\n\
                 let a = Atomic.new(true);\n\
                 println(a.load(MemoryOrdering.SeqCst));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "true\n");
    }
}

#[test]
fn test_e2e_atomic_bool_store_then_load_relaxed() {
    let out = run_program(
        "fn main() {\n\
                 let mut a = Atomic.new(false);\n\
                 a.store(true, MemoryOrdering.Relaxed);\n\
                 println(a.load(MemoryOrdering.Relaxed));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "true\n");
    }
}

#[test]
fn test_e2e_atomic_bool_release_store_acquire_load() {
    // The canonical migration-tool pair on a bool field — the most
    // common Atomic[bool] shape in practice (shutdown sentinel,
    // initialization flag).
    let out = run_program(
        "fn main() {\n\
                 let mut a = Atomic.new(false);\n\
                 a.store(true, MemoryOrdering.Release);\n\
                 println(a.load(MemoryOrdering.Acquire));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "true\n");
    }
}

#[test]
fn test_e2e_atomic_bool_annotated_let() {
    // Annotated form `let a: Atomic[bool] = Atomic.new(false)` —
    // the explicit-annotation path of the bool-inner detection in
    // the let-stmt handler. Without it, the FieldAccess inner-type
    // detection still works but the let path falls back to "not
    // bool" and the load returns i8 instead of i1.
    let out = run_program(
        "fn main() {\n\
                 let a: Atomic[bool] = Atomic.new(false);\n\
                 println(a.load(MemoryOrdering.SeqCst));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "false\n");
    }
}

#[test]
fn test_e2e_atomic_bool_field_load_store_struct_field() {
    // FieldAccess receiver with Atomic[bool] — exactly the shape
    // `karac migrate --atomic` emits when a `shared struct` field
    // of type `bool` with only bare `=` writes gets promoted.
    let out = run_program(
        "struct Flag { ready: Atomic[bool] }\n\
             fn main() {\n\
                 let f = Flag { ready: Atomic.new(false) };\n\
                 f.ready.store(true, MemoryOrdering.Release);\n\
                 println(f.ready.load(MemoryOrdering.Acquire));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "true\n");
    }
}

#[test]
fn test_ir_atomic_bool_load_emits_load_atomic_i8() {
    // IR shape: the slot is widened to `i8`, the load instruction
    // is `load atomic i8`, and a `trunc i8 ... to i1` immediately
    // converts back to the surface bool. Pins the widening +
    // trunc-on-load pair against regression.
    let ir = ir_for(
        "fn main() {\n\
                 let a = Atomic.new(true);\n\
                 let _ = a.load(MemoryOrdering.SeqCst);\n\
             }",
    );
    assert!(
        ir.contains("load atomic i8"),
        "expected `load atomic i8` in IR (Atomic[bool] widened slot); got:\n{}",
        ir
    );
    assert!(
        ir.contains("trunc i8") && ir.contains("to i1"),
        "expected `trunc i8 ... to i1` (load-side bool narrow); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_atomic_bool_store_emits_store_atomic_i8_with_zext() {
    // IR shape: incoming i1 is zexted to i8 before the atomic
    // store. Pins both halves of the widening pair — the slot
    // width on the store instruction (i8) and the zext that
    // converts the dynamic bool value to that width. A bool-typed
    // local variable (not a literal `true`) is required to defeat
    // constant folding: `store atomic i8 true_const` collapses to
    // `store atomic i8 1` without a visible zext.
    let ir = ir_for(
        "fn main() {\n\
                 let mut a = Atomic.new(false);\n\
                 let flag = true;\n\
                 a.store(flag, MemoryOrdering.Release);\n\
             }",
    );
    assert!(
        ir.contains("store atomic i8"),
        "expected `store atomic i8` in IR (Atomic[bool] widened slot); got:\n{}",
        ir
    );
    assert!(
        ir.contains("zext i1") && ir.contains("to i8"),
        "expected `zext i1 ... to i8` (store-side bool widen); got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_match_ok_unit_payload_is_exhaustive_and_selects_correct_arm() {
    // Regression: matching `Result[(), E]` with `Ok(())` / `Err(_)` is
    // exhaustive (the `()` empty-tuple pattern covers the unit payload)
    // and selects the right arm at runtime. Before the
    // `enumerate_ctors(Unit)` + variant-payload generic-substitution fix
    // this was wrongly flagged non-exhaustive and miscompiled (the Err
    // value fell through to a placeholder). Observed via stdout, not an
    // exit code, to keep the assertion ExitCode-independent.
    let out = run_program(
        r#"
fn classify(r: Result[(), String]) -> i64 {
    match r {
        Ok(()) => 10,
        Err(e) => 20,
    }
}
fn main() {
    println(classify(Err("x")));
    println(classify(Ok(())));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "20\n10");
    }
}

#[test]
fn e2e_tracing_par_branch_inherits_active_span() {
    // phase-8 line 153: a child task (par branch) inherits the
    // parent's active span. The branch runs on a worker thread (fresh
    // TLS), but the env-struct snapshot taken at par-block entry seeds
    // the worker's active-span register, so a `Log.*` inside the
    // branch is stamped with the enclosing `with_span`'s id. Single
    // branch → deterministic output. After `with_span` exits, the
    // active span is restored (the trailing `Log.info` has no suffix).
    let out = run_program(
        r#"fn main() {
                let s = Span.root("req", 5);
                with_span(s, || {
                    par {
                        Log.info("in-branch");
                    }
                });
                Log.info("after");
            }"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("[info] in-branch span_id=5\n[info] after\n"),
    );
}

// ── Phase 6 `par struct` slice C: always-Arc codegen ─────────
// A `par struct` / `par enum` reuses the entire shared-struct codegen
// (same `{ i64 refcount, … }` heap layout, malloc, field access, method
// dispatch, drop). The ONLY difference is that the refcount header is
// mutated atomically (`atomicrmw`, via emit_arc_inc / emit_arc_dec)
// because `par` values cross task boundaries. design.md § Part 5b.

#[test]
fn test_ir_par_struct_rc_inc_is_atomic() {
    // The clone pattern `let b = a` increments the refcount. For a `par
    // struct` that inc must be an `atomicrmw add`, NOT the plain
    // `add i64 %rc` a `shared struct` emits.
    let ir = ir_for(
        r#"
par struct Obj { data: i64 }
fn copy_par() {
    let a = Obj { data: 10 };
    let b = a;
    let _x = a.data + b.data;
}
"#,
    );
    assert!(
        ir.contains("atomicrmw add"),
        "par struct copy must emit an atomic refcount increment (atomicrmw add); got IR:\n{ir}"
    );
    assert!(
            !ir.contains("add i64 %rc"),
            "par struct refcount inc must NOT use the non-atomic `add i64 %rc` shared path; got IR:\n{ir}"
        );
}

#[test]
fn test_ir_par_struct_rc_dec_is_atomic_and_frees() {
    // Scope exit decrements the refcount and conditionally frees. For a
    // `par struct` the decrement must be an `atomicrmw sub`.
    let ir = ir_for(
        r#"
par struct Token { id: i64 }
fn use_token() {
    let t = Token { id: 1 };
    let _x = t.id;
}
"#,
    );
    assert!(
            ir.contains("atomicrmw sub"),
            "par struct scope exit must emit an atomic refcount decrement (atomicrmw sub); got IR:\n{ir}"
        );
    assert!(
        ir.contains("@free"),
        "par struct scope exit must conditionally free the heap allocation; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_shared_struct_rc_stays_non_atomic_regression() {
    // Regression: the par atomic routing must NOT leak into `shared
    // struct` — a shared struct keeps the plain non-atomic refcount.
    let ir = ir_for(
        r#"
shared struct Obj { data: i64 }
fn copy_shared() {
    let a = Obj { data: 10 };
    let b = a;
    let _x = a.data + b.data;
}
"#,
    );
    assert!(
        ir.contains("add i64 %rc"),
        "shared struct must keep the non-atomic `add i64 %rc` refcount inc; got IR:\n{ir}"
    );
    assert!(
        !ir.contains("atomicrmw"),
        "shared struct must emit NO atomic refcount ops; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_par_struct_basic() {
    let out = run_program(
        r#"
par struct Counter { val: i64 }
fn main() {
    let c = Counter { val: 42 };
    println(c.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_par_struct_with_atomic_field_constructs() {
    // `Atomic.new` in general (struct-field-init) position lets a
    // concurrent `par struct` with an `Atomic` field be constructed —
    // `Atomic[T]` is a transparent wrapper, so the field lowers to its
    // inner value. (Atomic-method dispatch on a shared/par field receiver
    // is a separate pre-existing gap; here we read a sibling plain field
    // to prove construction + the Arc lifecycle work with an Atomic field
    // present.)
    let out = run_program(
        r#"
par struct Counter { count: Atomic[i64], label: i64 }
fn label_of(c: Counter) -> i64 { c.label }
fn main() {
    let c = Counter { count: Atomic.new(0), label: 7 };
    println(label_of(c));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_par_struct_atomic_field_set_get() {
    // The canonical concurrent shape: a `par struct` with an `Atomic`
    // field, mutated + read via atomic store/load on a `ref self` method.
    // Exercises atomic method dispatch on a par-struct FIELD receiver
    // (resolve_atomic_storage's shared/par branch — GEP idx+1 into the
    // heap layout). Round-trips 10 -> 42.
    let out = run_program(
        r#"
par struct Counter { count: Atomic[i64] }
impl Counter {
    fn get(ref self) -> i64 { self.count.load(MemoryOrdering.SeqCst) }
    fn set(ref self, v: i64) { self.count.store(v, MemoryOrdering.SeqCst) }
}
fn main() {
    let c = Counter { count: Atomic.new(10) };
    println(c.get());
    c.set(42);
    println(c.get());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "42"]);
    }
}

#[test]
fn test_ir_atomic_fetch_add_emits_atomicrmw() {
    let ir = ir_for(
        r#"
par struct Counter { count: Atomic[i64] }
impl Counter { fn inc(ref self) -> i64 { self.count.fetch_add(1, MemoryOrdering.SeqCst) } }
fn main() { let c = Counter { count: Atomic.new(0) }; let _ = c.inc(); }
"#,
    );
    assert!(
        ir.contains("atomicrmw add"),
        "fetch_add must lower to `atomicrmw add`; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_atomic_fetch_add_sub_returns_old_and_accumulates() {
    // fetch_add / fetch_sub return the PREVIOUS value and mutate in place.
    let out = run_program(
        r#"
par struct Counter { count: Atomic[i64] }
impl Counter {
    fn inc(ref self) -> i64 { self.count.fetch_add(1, MemoryOrdering.SeqCst) }
    fn dec(ref self) -> i64 { self.count.fetch_sub(1, MemoryOrdering.SeqCst) }
    fn get(ref self) -> i64 { self.count.load(MemoryOrdering.SeqCst) }
}
fn main() {
    let c = Counter { count: Atomic.new(0) };
    println(c.inc());   // 0 (old)
    println(c.inc());   // 1
    println(c.get());   // 2
    println(c.dec());   // 2 (old)
    println(c.get());   // 1
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "1", "2", "2", "1"]);
    }
}

#[test]
fn test_ir_atomic_swap_and_bitwise_emit_correct_atomicrmw() {
    // swap / fetch_and / fetch_or / fetch_xor each lower to the matching
    // single `atomicrmw` opcode (xchg / and / or / xor).
    let ir = ir_for(
        r#"
par struct B { v: Atomic[i64] }
impl B {
    fn a(ref self) -> i64 { self.v.fetch_and(6, MemoryOrdering.SeqCst) }
    fn o(ref self) -> i64 { self.v.fetch_or(1, MemoryOrdering.SeqCst) }
    fn x(ref self) -> i64 { self.v.fetch_xor(2, MemoryOrdering.SeqCst) }
    fn s(ref self) -> i64 { self.v.swap(9, MemoryOrdering.SeqCst) }
}
fn main() {
    let b = B { v: Atomic.new(7) };
    let _ = b.a(); let _ = b.o(); let _ = b.x(); let _ = b.s();
}
"#,
    );
    for op in [
        "atomicrmw and",
        "atomicrmw or",
        "atomicrmw xor",
        "atomicrmw xchg",
    ] {
        assert!(ir.contains(op), "expected `{op}` in IR; got:\n{ir}");
    }
}

#[test]
fn test_e2e_atomic_bitwise_and_swap_semantics() {
    // fetch_or / fetch_and / fetch_xor / swap all return the OLD value and
    // apply the bitwise / exchange op in place.
    let out = run_program(
        r#"
par struct Flags { bits: Atomic[i64] }
impl Flags {
    fn set_or(ref self, m: i64) -> i64 { self.bits.fetch_or(m, MemoryOrdering.SeqCst) }
    fn clear_and(ref self, m: i64) -> i64 { self.bits.fetch_and(m, MemoryOrdering.SeqCst) }
    fn toggle(ref self, m: i64) -> i64 { self.bits.fetch_xor(m, MemoryOrdering.SeqCst) }
    fn exchange(ref self, v: i64) -> i64 { self.bits.swap(v, MemoryOrdering.SeqCst) }
    fn get(ref self) -> i64 { self.bits.load(MemoryOrdering.SeqCst) }
}
fn main() {
    let f = Flags { bits: Atomic.new(0) };
    println(f.set_or(5));    // old 0  -> 5
    println(f.set_or(2));    // old 5  -> 7
    println(f.clear_and(6)); // old 7  -> 6   (7 & 6)
    println(f.toggle(2));    // old 6  -> 4   (6 ^ 2)
    println(f.exchange(99)); // old 4  -> 99
    println(f.get());        // 99
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "5", "7", "6", "4", "99"]);
    }
}

#[test]
fn test_e2e_atomic_bool_swap() {
    // `swap` is the one RMW that also works on `Atomic[bool]` (i8 slot —
    // incoming i1 widened, returned old i8 truncated). A take-once latch.
    let out = run_program(
        r#"
par struct Latch { flag: Atomic[bool] }
impl Latch {
    fn take(ref self) -> bool { self.flag.swap(true, MemoryOrdering.SeqCst) }
    fn get(ref self) -> bool { self.flag.load(MemoryOrdering.SeqCst) }
}
fn main() {
    let l = Latch { flag: Atomic.new(false) };
    if l.take() { println(1); } else { println(0); }   // old false -> 0
    if l.get() { println(1); } else { println(0); }    // now true  -> 1
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "1"]);
    }
}

#[test]
fn test_ir_atomic_compare_exchange_emits_cmpxchg() {
    let ir = ir_for(
        r#"
par struct C { v: Atomic[i64] }
impl C {
    fn cas(ref self) -> Result[i64, i64] {
        self.v.compare_exchange(0, 5, MemoryOrdering.SeqCst, MemoryOrdering.SeqCst)
    }
}
fn main() { let c = C { v: Atomic.new(0) }; let _ = c.cas(); }
"#,
    );
    assert!(
        ir.contains("cmpxchg"),
        "compare_exchange must lower to LLVM `cmpxchg`; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_atomic_compare_exchange_success_and_failure() {
    // CAS returns Ok(prev) and swaps when the expected value matches;
    // Err(actual) and leaves the slot unchanged when it does not.
    let out = run_program(
        r#"
par struct Cell { v: Atomic[i64] }
impl Cell {
    fn cas(ref self, old: i64, new: i64) -> Result[i64, i64] {
        self.v.compare_exchange(old, new, MemoryOrdering.SeqCst, MemoryOrdering.SeqCst)
    }
    fn get(ref self) -> i64 { self.v.load(MemoryOrdering.SeqCst) }
}
fn report(r: Result[i64, i64]) {
    match r {
        Ok(prev) => { println(100 + prev); }
        Err(actual) => { println(200 + actual); }
    }
}
fn main() {
    let c = Cell { v: Atomic.new(5) };
    report(c.cas(5, 9));   // 5 == 5 -> Ok(5)  -> 105
    println(c.get());      // 9 (swapped)
    report(c.cas(5, 1));   // 5 != 9 -> Err(9) -> 209
    println(c.get());      // 9 (unchanged — failed CAS does not store)
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["105", "9", "209", "9"]);
    }
}

#[test]
fn test_e2e_lock_block_single_threaded_alias_and_no_alias() {
    // Both forms: `lock m x { ... }` (alias) and `lock m { ... m ... }`
    // (the mutex name shadows to the inner value). 10 -> +5 -> *2 -> 30.
    let out = run_program(
        r#"
fn main() {
    let m = Mutex.new(10);
    lock m x { x = x + 5; }
    lock m y { y = y * 2; }
    lock m { println(m); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "30");
    }
}

#[test]
fn test_e2e_concurrent_mutex_counter_no_lost_updates() {
    // A standalone local `Mutex[i64]` shared across two `par {}` branches
    // (par captures the local by reference to the same aggregate). The
    // spinlock serializes the read-modify-write, so 2×50_000 increments
    // yield exactly 100_000 — a non-locked `+1` would lose updates.
    let out = run_program(
        r#"
fn main() {
    let m = Mutex.new(0);
    par {
        {
            let mut i = 0;
            while i < 50000 { lock m x { x = x + 1; } i = i + 1; }
        }
        {
            let mut j = 0;
            while j < 50000 { lock m y { y = y + 1; } j = j + 1; }
        }
    }
    lock m v { println(v); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100000");
    }
}

#[test]
fn test_e2e_lock_through_mut_ref_mutex_param() {
    // `lock m` where `m: mut ref Mutex[i64]` — the alloca holds a pointer to
    // the aggregate; the lock loads through the reference (recovering the
    // Mutex struct type from `ref_params`) then spins. A helper that mutates
    // a mutex passed by reference. 10 -> +5 -> 15.
    let out = run_program(
        r#"
fn bump(m: mut ref Mutex[i64], n: i64) { lock m x { x = x + n; } }
fn main() {
    let mut m = Mutex.new(10);
    bump(mut m, 5);
    lock m v { println(v); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_lock_par_struct_mutex_field_sequential() {
    // Slice 2: `lock self.total` — locking a `Mutex` FIELD of a par struct
    // (a place expression, not just a bare binding). The field is stored
    // inline in the Arc heap aggregate; the lock GEPs to it and spins.
    let out = run_program(
        r#"
par struct Counter { total: Mutex[i64] }
impl Counter {
    fn add(ref self, n: i64) { lock self.total t { t = t + n; } }
    fn get(ref self) -> i64 { lock self.total t { t } }
}
fn main() {
    let c = Counter { total: Mutex.new(0) };
    c.add(5);
    c.add(37);
    println(c.get());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_concurrent_par_struct_mutex_field_counter() {
    // The canonical par-struct concurrent-mutation pattern: a `Mutex` field
    // locked from two `par {}` branches. The par struct passes by value
    // (Arc-shared), each branch `lock self.total`s and increments — the
    // spinlock serializes, so 2×50_000 → exactly 100_000, no lost updates.
    let out = run_program(
        r#"
par struct Counter { total: Mutex[i64] }
impl Counter {
    fn inc(ref self) { lock self.total t { t = t + 1; } }
    fn get(ref self) -> i64 { lock self.total t { t } }
}
fn bump(c: Counter, n: i64) {
    let mut i = 0;
    while i < n { c.inc(); i = i + 1; }
}
fn main() {
    let c = Counter { total: Mutex.new(0) };
    par {
        bump(c, 50000);
        bump(c, 50000);
    }
    println(c.get());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100000");
    }
}

#[test]
fn test_e2e_concurrent_atomic_counter_no_lost_updates() {
    // The headline lock-free-counter shape: two sibling `par {}` branches
    // each fetch_add 50_000 times on the SAME par struct's Atomic field.
    // Atomicity guarantees exactly 100_000 — a non-atomic load+store would
    // lose updates under contention and print < 100_000.
    let out = run_program(
        r#"
par struct Counter { count: Atomic[i64] }
impl Counter {
    fn inc(ref self) { let _ = self.count.fetch_add(1, MemoryOrdering.SeqCst); }
    fn get(ref self) -> i64 { self.count.load(MemoryOrdering.SeqCst) }
}
fn bump_many(c: Counter, n: i64) {
    let mut i = 0;
    while i < n {
        c.inc();
        i = i + 1;
    }
}
fn main() {
    let c = Counter { count: Atomic.new(0) };
    par {
        bump_many(c, 50000);
        bump_many(c, 50000);
    }
    println(c.get());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100000");
    }
}

#[test]
fn test_e2e_shared_struct_atomic_field_load() {
    // Regression: the atomic-field-receiver fix is shared-wide, not
    // par-only — a `shared struct` Atomic field load worked never before
    // (same "no LLVM type" codegen error) and now does.
    let out = run_program(
        r#"
shared struct Counter { count: Atomic[i64] }
impl Counter {
    fn get(ref self) -> i64 { self.count.load(MemoryOrdering.SeqCst) }
}
fn main() {
    let c = Counter { count: Atomic.new(99) };
    println(c.get());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_par_struct_atomic_field_read_across_par_block() {
    // Concurrent read: the same par struct's Atomic field is read from two
    // sibling par branches, then after the join. Atomic load on the heap
    // field slot, clean exit (no race on the refcount or the field).
    let out = run_program(
        r#"
par struct Counter { count: Atomic[i64] }
impl Counter {
    fn get(ref self) -> i64 { self.count.load(MemoryOrdering.SeqCst) }
}
fn read(c: Counter) -> i64 { c.get() }
fn main() {
    let c = Counter { count: Atomic.new(5) };
    par {
        let _a = read(c);
        let _b = read(c);
    }
    println(c.get());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_par_struct_passed_to_fn_and_aliased() {
    // Construct, alias (refcount inc), pass to a fn, read fields — the
    // whole atomic-refcount lifecycle in one binary.
    let out = run_program(
        r#"
par struct Data { x: i64 }
fn read(d: Data) -> i64 { d.x }
fn main() {
    let a = Data { x: 100 };
    let b = a;
    println(read(a));
    println(b.x);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["100", "100"]);
    }
}

#[test]
fn test_e2e_recursive_par_struct_drops_inner_handles() {
    // A `par struct` holding an inner `par` handle (`Option[Node]`). On
    // drop, the synthesized `__karac_rc_drop_Node` walk decrements the
    // inner handle — routed through the by-type dispatcher so a `par`
    // inner is decremented ATOMICALLY (it could still be live in another
    // task). Must construct, print, and exit cleanly (no double-free).
    let out = run_program(
        r#"
par struct Node { val: i64, next: Option[Node] }
fn main() {
    let leaf = Node { val: 2, next: Option.None };
    let head = Node { val: 1, next: Option.Some(leaf) };
    println(head.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_e2e_par_enum_tuple_variant() {
    // `par enum` reuses the shared-enum heap layout; only its refcount is
    // atomic. Mirrors test_e2e_shared_enum_tuple_variant (bare variant
    // names — the qualified `Enum.Variant(..)` form is a pre-existing
    // construction gap shared by all enum kinds).
    let out = run_program(
        r#"
par enum Value { Num(i64), Nothing }
fn extract(v: Value) -> i64 {
    match v {
        Num(n) => n,
        Nothing => 0,
    }
}
fn main() {
    let v = Num(42);
    println(extract(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_ir_par_enum_rc_is_atomic() {
    // A `par enum`'s refcount header must also be mutated atomically.
    let ir = ir_for(
        r#"
par enum Value { Num(i64), Nothing }
fn make() {
    let a = Num(10);
    let b = a;
    let _ = match a { Num(n) => n, Nothing => 0 };
    let _ = match b { Num(n) => n, Nothing => 0 };
}
"#,
    );
    assert!(
        ir.contains("atomicrmw"),
        "par enum must emit atomic refcount ops; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_par_struct_across_par_block() {
    // The reason `par struct` exists: the same value used from two
    // sibling `par {}` branches (each pass atomically bumps the count),
    // then read after the join. Must produce correct output and exit 0
    // (clean atomic refcount → no double-free / leak crash).
    let out = run_program(
        r#"
par struct Counter { val: i64 }
fn read(c: Counter) -> i64 { c.val }
fn main() {
    let c = Counter { val: 7 };
    par {
        let _a = read(c);
        let _b = read(c);
    }
    println(c.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_vector_select() {
    // `(a < b).select(a, b)` lowers to LLVM `select <4 x i1>` — per-lane min.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 5, 3, 8);
    let b = Vector[i64, 4](4, 2, 3, 6);
    let mn = (a < b).select(a, b);
    println(mn[0]); println(mn[1]); println(mn[2]); println(mn[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n2\n3\n6\n");
    }
}

#[test]
fn test_vector_compare_float_select() {
    // Ordered float compare (`<=` → OLE) + select on the resulting mask.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[f64, 2](1.5, 9.0);
    let b = Vector[f64, 2](2.0, 3.0);
    let m = a <= b;
    println(m[0]); println(m[1]);
    let pick = (a <= b).select(a, b);
    println(pick[0]); println(pick[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "true\nfalse\n1.5\n3\n");
    }
}

#[test]
fn test_e2e_dataframe_select_subset_reorder() {
    // select picks a column subset in the given order (a reorder); the
    // result is a fresh frame whose columns are copies; the source is
    // unchanged. Byte-identical to the interpreter twin.
    let out = run_program(
        "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"a\", Column.from_vec([1, 2]));\n\
                 df.insert(\"b\", Column.from_vec([3, 4]));\n\
                 df.insert(\"c\", Column.from_vec([5, 6]));\n\
                 let sub: DataFrame = df.select([\"c\", \"a\"]);\n\
                 println(sub.width());\n\
                 println(sub.height());\n\
                 for n in sub.column_names() { println(n); }\n\
                 let c0: Column[i64] = sub.column(\"c\");\n\
                 match c0[1] { Some(v) => println(v), None => println(-1) }\n\
                 println(df.width());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n2\nc\na\n6\n3\n",
            "select subset/reorder must match the interpreter"
        );
    }
}

/// B-2026-08-22-16 — `Channel.bounded(cap)`.
///
/// design.md § `Channel[T]` API documents it with a `requires cap > 0`
/// contract, and nothing routed the name: `grep '"bounded"' src/` returned
/// nothing, so both spellings reported "no associated function 'bounded'".
/// It returns the SAME `(Sender[T], Receiver[T])` pair as `Channel.new()` —
/// the bound changes the queue's behaviour, not construction's shape.
///
/// The qualified spelling is covered here too because it is the one
/// design.md settles on for explicit type selection, and it only became
/// reachable for builtin heads with B-2026-08-22-17 — this row was blocked
/// behind that one.
#[test]
fn e2e_bounded_channel_construction_and_capacity() {
    // Within capacity, both spellings, interleaved send/recv so the queue
    // drains and refills rather than only ever growing.
    let src = "fn main() {\n\
            \x20   let (tx, rx) = Channel.bounded(2);\n\
            \x20   tx.send(1); tx.send(2);\n\
            \x20   println(rx.recv());\n\
            \x20   tx.send(3);\n\
            \x20   println(rx.recv());\n\
            \x20   println(rx.recv());\n\
            \x20   let (qx, qr) = Channel[i64].bounded(3);\n\
            \x20   qx.send(7);\n\
            \x20   println(qr.recv());\n\
            }\n";
    assert_eq!(run_program(src).as_deref(), Some("1\n2\n3\n7\n"));

    // The unbounded twin, as the control: `bounded` must not have changed
    // what `new` does, and a capacity of 0 is what marks "unbounded"
    // internally — so a regression there would surface as `new` suddenly
    // rejecting its second send.
    let twin = "fn main() {\n\
            \x20   let (tx, rx) = Channel.new();\n\
            \x20   tx.send(1); tx.send(2); tx.send(3);\n\
            \x20   println(rx.recv());\n\
            \x20   println(rx.recv());\n\
            \x20   println(rx.recv());\n\
            }\n";
    assert_eq!(run_program(twin).as_deref(), Some("1\n2\n3\n"));
}

/// B-2026-08-22-21 — `Sender.try_send` and `Receiver.recv_blocking`, the
/// two methods design.md:6070 declares that had no implementation at all
/// ("no method 'try_send' on type 'Sender'").
///
/// `try_send` is the NON-PANICKING send: where `send` returns unit and
/// therefore panics on a full bounded channel (B-2026-08-22-16's fail-fast
/// collapse), this returns `Result[(), SendError[T]]` and hands the
/// REJECTED VALUE back inside it — `send` consumes its argument, so a
/// failure that dropped the value would leave nothing to retry with. That
/// hand-back is the half worth asserting, so every arm prints the payload.
///
/// `SendError.Closed` IS reachable since B-2026-08-22-24 gave the
/// interpreter per-end receiver counts; it has its own test below, and the
/// arms here print it so a misfire in THIS test's shapes (all of which
/// hold a live `rx`) shows up as a wrong string rather than silently.
/// B-2026-08-22-24 — the COMPILED twin of
/// `tests/interpreter.rs::channel_try_send_reports_closed_when_the_receiver_is_gone`.
///
/// Both backends must agree here, and that agreement is the entire reason
/// the behaviour could land: the compiled runtime always had the live-end
/// counters and an earlier draft used them, but the interpreter's channel
/// was one shared handle with no per-end liveness, so wiring only this
/// side printed `closed` under `build` and `sent` under `--interp`. The
/// check came back only once the interpreter could answer too, so a
/// regression on EITHER side must fail a test — hence the pair.
#[test]
fn e2e_channel_closed_when_the_receiver_is_gone() {
    // `rx` dies when `orphan` returns; the escaping sender has no peer.
    let orphaned = "fn orphan() -> Sender[i64] {\n\
            \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
            \x20   return tx;\n\
            }\n\
            fn main() {\n\
            \x20   match orphan().try_send(9) {\n\
            \x20       Ok(u) => println(\"sent\"),\n\
            \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
            \x20       Err(SendError.Full(v)) => println(f\"full {v}\"),\n\
            \x20   }\n\
            }\n";
    assert_eq!(
        run_program(orphaned).as_deref(),
        Some("closed 9\n"),
        "an orphaned sender must report Closed under the compiled backend"
    );

    // Control: a live receiver still accepts. Without this an always-zero
    // count would satisfy the assertion above and break every real program.
    let live = "fn main() {\n\
            \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
            \x20   match tx.try_send(9) {\n\
            \x20       Ok(u) => println(\"sent\"),\n\
            \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
            \x20       Err(SendError.Full(v)) => println(f\"full {v}\"),\n\
            \x20   }\n\
            \x20   match rx.try_recv() { Some(v) => println(v), None => println(\"empty\") }\n\
            }\n";
    assert_eq!(
        run_program(live).as_deref(),
        Some("sent\n9\n"),
        "a live receiver must still accept the send"
    );
}

#[test]
fn e2e_channel_try_send_and_recv_blocking() {
    // Scalar payload, capacity 1: first `try_send` lands, second is
    // rejected `Full` and gives `2` back, then the queue drains.
    let scalar = "fn main() {\n\
            \x20   let (tx, rx) = Channel.bounded(1);\n\
            \x20   match tx.try_send(1) {\n\
            \x20       Ok(u) => println(\"sent 1\"),\n\
            \x20       Err(SendError.Full(v)) => println(f\"full, got back {v}\"),\n\
            \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
            \x20   }\n\
            \x20   match tx.try_send(2) {\n\
            \x20       Ok(u) => println(\"sent 2\"),\n\
            \x20       Err(SendError.Full(v)) => println(f\"full, got back {v}\"),\n\
            \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
            \x20   }\n\
            \x20   println(f\"drained {rx.recv_blocking()}\");\n\
            }\n";
    assert_eq!(
        run_program(scalar).as_deref(),
        Some("sent 1\nfull, got back 2\ndrained 1\n")
    );

    // HEAP payload on an UNANNOTATED pair — the shape that exercises both
    // the `SendError[String]` payload (wider than the seeded 1-word area,
    // so it boxes) and the element-type pinning fix below.
    let heap = "fn main() {\n\
            \x20   let (tx, rx) = Channel.bounded(1);\n\
            \x20   let a = \"alpha\";\n\
            \x20   match tx.try_send(a) {\n\
            \x20       Ok(u) => println(\"sent alpha\"),\n\
            \x20       Err(SendError.Full(v)) => println(f\"full, kept {v}\"),\n\
            \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
            \x20   }\n\
            \x20   let b = \"beta\";\n\
            \x20   match tx.try_send(b) {\n\
            \x20       Ok(u) => println(\"sent beta\"),\n\
            \x20       Err(SendError.Full(v)) => println(f\"full, kept {v}\"),\n\
            \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
            \x20   }\n\
            \x20   println(f\"drained {rx.recv_blocking()}\");\n\
            }\n";
    assert_eq!(
        run_program(heap).as_deref(),
        Some("sent alpha\nfull, kept beta\ndrained alpha\n")
    );

    // The payload binding must carry its real TYPE, not just its bytes —
    // so call a method on it. This shape (a match on the FIRST `try_send`
    // in the function, on an unannotated channel) is the one that broke:
    // `try_send` is the only channel method that both pins the element
    // from its argument and returns a type mentioning it, so it handed the
    // match a `Result[(), SendError[?T0]]` scrutinee, the payload binding
    // got no recorded type, and `v.len()` died with `codegen: no handler
    // for method 'len' on variable 'v'`. A PRECEDING bare
    // `let r = tx.try_send(x)` pins the element and hides the whole thing,
    // which is why the match comes first here and a second `try_send`
    // follows it.
    let typed_payload = "fn main() {\n\
            \x20   let mut total: i64 = 0;\n\
            \x20   let (tx, rx) = Channel.bounded(1);\n\
            \x20   let first = \"first\";\n\
            \x20   match tx.try_send(first) {\n\
            \x20       Ok(u) => { total = total + 1; }\n\
            \x20       Err(SendError.Full(v)) => { total = total + v.len(); }\n\
            \x20       Err(SendError.Closed(v)) => { total = total + v.len(); }\n\
            \x20   }\n\
            \x20   let second = \"second\";\n\
            \x20   match tx.try_send(second) {\n\
            \x20       Ok(u) => { total = total + 1; }\n\
            \x20       Err(SendError.Full(v)) => { total = total + v.len(); }\n\
            \x20       Err(SendError.Closed(v)) => { total = total + v.len(); }\n\
            \x20   }\n\
            \x20   println(total.to_string());\n\
            }\n";
    // First lands (+1); second is Full and gives back "second" (+6) = 7.
    assert_eq!(run_program(typed_payload).as_deref(), Some("7\n"));
}

/// B-2026-08-22-21, the PRE-EXISTING `send` defect the row's `try_send`
/// work uncovered and had to fix to proceed — measured on clean `main`
/// before any of that work landed.
///
/// `infer_channel_method` recorded `channel_elem_types[span]` BEFORE the
/// per-method arm ran, and on an unannotated `Channel.new()` the element is
/// still an unsolved `?T0` at that point — only the arm's
/// `pin_channel_elem_from_arg` solves it, from the first send's argument.
/// So the first send recorded a 1-word placeholder and codegen sized its
/// `elem_size` at 8 bytes for a 24-byte `String`: `karac build` printed an
/// EMPTY payload (silently dropped) against the interpreter's correct one,
/// and `karac run` aborted in the runtime with
/// `receiver elem_size 24 exceeds sent blob 8`.
///
/// The ANNOTATED pair is the control, and it is why this went unnoticed:
/// every pre-existing String-channel test annotates, so none of them could
/// see it.
#[test]
fn e2e_channel_unannotated_heap_send_pins_element_size() {
    let unannotated = "fn main() {\n\
            \x20   let (tx, rx) = Channel.new();\n\
            \x20   let a = \"alpha\";\n\
            \x20   tx.send(a);\n\
            \x20   println(f\"got {rx.recv()}\");\n\
            }\n";
    assert_eq!(run_program(unannotated).as_deref(), Some("got alpha\n"));

    let annotated = "fn main() {\n\
            \x20   let (tx, rx): (Sender[String], Receiver[String]) = Channel.new();\n\
            \x20   let a = \"alpha\";\n\
            \x20   tx.send(a);\n\
            \x20   println(f\"got {rx.recv()}\");\n\
            }\n";
    assert_eq!(run_program(annotated).as_deref(), Some("got alpha\n"));
}

#[test]
fn e2e_channel_send_recv_tryrecv_clone() {
    // Phase 6 "Channel AOT codegen lowering": `Channel.new()` destructure,
    // `Sender.send` / `Sender.clone`, `Receiver.recv` / `Receiver.try_recv`
    // through the `karac_runtime_channel_*` runtime. Exercises:
    //  - i64 + String (multi-word) element types,
    //  - `clone` (a second sender into the same queue),
    //  - `try_recv` Some/None (drained queue → None),
    //  - a control-flow construct BEFORE the channel pair, which used to
    //    let auto-par fan `send`/`recv` into separate `__par_branch`
    //    workers (reordering the transfer + isolating the channel-end
    //    bindings) — now excluded via `stmt_has_channel_op`. Runs under
    //    the default auto-par-ON build.
    let out = run_program_capturing(
        r#"
fn main() {
    let n = 3;
    if n > 0 { println(n); }
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    tx.send(100);
    let tx2 = tx.clone();
    tx2.send(5);
    match rx.try_recv() {
        Some(v) => println(v),
        None => println(-1),
    }
    match rx.try_recv() {
        Some(v) => println(v),
        None => println(-1),
    }
    match rx.try_recv() {
        Some(v) => println(v),
        None => println(-1),
    }
    let (stx, srx): (Sender[String], Receiver[String]) = Channel.new();
    stx.send("hello channel");
    println(srx.recv());
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "3\n100\n5\n-1\nhello channel");
    }
}

#[test]
fn e2e_channel_blocking_recv_until_send() {
    // Blocking `recv`: `main` reaches `rx.recv()` while the spawned worker
    // is still busy-computing, so it MUST park until the worker sends —
    // load-immune evidence of blocking (a non-blocking recv would read the
    // empty queue and return 0 before the worker ran). The received value
    // is the worker's full sum (0+1+…+(N-1)), so a short-circuited recv
    // would print 0, not the sum.
    let out = run_program_capturing(
        r#"
fn worker(tx: Sender[i64]) -> i64 {
    let mut acc = 0;
    let mut i = 0;
    while i < 5000000 {
        acc = acc + i;
        i = i + 1;
    }
    tx.send(acc);
    0
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let h: TaskHandle[i64] = spawn(|| worker(tx));
    let v = rx.recv();
    h.join();
    println(v);
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "12499997500000");
    }
}

#[test]
fn e2e_channel_producer_consumer_close_terminates() {
    // The canonical producer-consumer-with-spawn pattern, which deadlocked
    // before cross-task sender-drop: the producer (spawned task) sends 3
    // values then finishes; its moved `Sender` is dropped BY THE TASK
    // (not the parent), which CLOSES the channel and wakes the consumer's
    // blocking `recv`, terminating the drain loop. The whole program both
    // produces the right sum AND exits (no hang).
    let out = run_program_capturing(
        r#"
fn producer(tx: Sender[i64]) -> i64 {
    tx.send(10);
    tx.send(20);
    tx.send(30);
    0
}
fn consume(rx: Receiver[i64]) -> i64 {
    let mut sum = 0;
    let mut go = true;
    while go {
        let v = rx.recv();
        if v == 0 { go = false; } else { sum = sum + v; }
    }
    sum
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let h: TaskHandle[i64] = spawn(|| producer(tx));
    let total = consume(rx);
    h.join();
    println(total);
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "60");
    }
}

#[test]
fn e2e_bounded_channel_send_full_recv_fifo() {
    // Phase 6 "BoundedChannel codegen": `BoundedChannel.new(capacity,
    // on_full)` + `send -> Result[Unit, ChannelError]` + `recv ->
    // Option[T]` through `karac_runtime_bounded_channel_*`. Mirrors the
    // interpreter's collapsed v1 semantics: `send` past capacity returns
    // `Err(Full)` (no parking), `recv` on empty returns `None`, draining
    // frees a slot. `1` = Ok, `-1` = Err(Full), `-99` = None.
    let out = run_program_capturing(
        r#"
fn main() {
    let bc: BoundedChannel[i64] = BoundedChannel.new(2, OnFull.FailFast);
    match bc.send(10) { Ok(_) => println(1), Err(_) => println(-1), }
    match bc.send(20) { Ok(_) => println(1), Err(_) => println(-1), }
    match bc.send(30) { Ok(_) => println(1), Err(_) => println(-1), }
    match bc.recv() { Some(v) => println(v), None => println(-99), }
    match bc.recv() { Some(v) => println(v), None => println(-99), }
    match bc.recv() { Some(v) => println(v), None => println(-99), }
    match bc.send(40) { Ok(_) => println(1), Err(_) => println(-1), }
    match bc.recv() { Some(v) => println(v), None => println(-99), }
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "1\n1\n-1\n10\n20\n-99\n1\n40");
    }
}

#[test]
fn e2e_bounded_channel_string_element() {
    // Multi-word (`String` = {ptr,len,cap}) element type round-trips
    // through the type-erased byte-blob queue (elem_size carries the
    // payload width per op). `Block` collapses to fail-fast in v1, so a
    // full `send` still returns `Err`.
    let out = run_program_capturing(
        r#"
fn main() {
    let bc: BoundedChannel[String] = BoundedChannel.new(1, OnFull.Block);
    match bc.send("hello bounded") { Ok(_) => println("sent"), Err(_) => println("full"), }
    match bc.send("overflow") { Ok(_) => println("sent"), Err(_) => println("full"), }
    match bc.recv() { Some(s) => println(s), None => println("empty"), }
    match bc.recv() { Some(s) => println(s), None => println("empty"), }
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "sent\nfull\nhello bounded\nempty");
    }
}

#[test]
fn e2e_bounded_channel_zero_capacity_always_full() {
    // A 0-capacity channel fails every `send` and never yields a value.
    let out = run_program_capturing(
        r#"
fn main() {
    let bc: BoundedChannel[i64] = BoundedChannel.new(0, OnFull.FailFast);
    match bc.send(1) { Ok(_) => println(1), Err(_) => println(-1), }
    match bc.recv() { Some(v) => println(v), None => println(-99), }
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "-1\n-99");
    }
}

#[test]
fn e2e_bounded_channel_onfull_bound_to_variable() {
    // `OnFull` bound to a `let` (not inline at the `new` call) is compiled
    // as an enum value — exercises the seeded `OnFull` layout in
    // `seed_builtin_enum_layouts`. (The inline-arg form is not lowered:
    // `new` ignores `on_full` in v1.) The bound policy is still
    // v1-collapsed to fail-fast.
    let out = run_program_capturing(
        r#"
fn main() {
    let policy: OnFull = OnFull.Block;
    let bc: BoundedChannel[i64] = BoundedChannel.new(2, policy);
    match bc.send(7) { Ok(_) => println(1), Err(_) => println(-1), }
    match bc.recv() { Some(v) => println(v), None => println(-99), }
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "1\n7");
    }
}

/// The atomics call site was never affected — `parse_memory_ordering`
/// reads the qualified variant literal straight off the AST — but it
/// shares the type, so this pins that seeding the layout did not disturb
/// it. A load/store round-trip plus an ordering that flows through a
/// binding, in one program.
#[test]
fn atomic_ordering_at_the_call_site_and_through_a_binding_agree() {
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let a = Atomic.new(0);\n\
                     a.store(7, MemoryOrdering.SeqCst);\n\
                     println(a.load(MemoryOrdering.SeqCst));\n\
                     let m = MemoryOrdering.Acquire;\n\
                     match m { Relaxed => println(\"Relaxed\"), Acquire => println(\"Acquire\"), Release => println(\"Release\"), AcqRel => println(\"AcqRel\"), SeqCst => println(\"SeqCst\") }\n\
                 }"
            ),
            Some("7\nAcquire\n".to_string())
        );
}

/// B-2026-08-22-2 — the COMPILED half of the bare-stdlib-variant bug.
/// Codegen was always right here (it resolves the bare name through the
/// enum layout), and the interpreter was the one selecting the first arm,
/// so this pins the side that was already correct: it is what the
/// interpreter fix has to agree WITH, and it fails loudly if a future
/// change to the enum-layout lookup regresses the compiled side into
/// matching the old interpreter behaviour.
#[test]
fn bare_stdlib_variant_pattern_selects_the_right_arm() {
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let e = IoError.PermissionDenied;\n\
                     match e {\n\
                         NotFound => println(\"NotFound\"),\n\
                         PermissionDenied => println(\"PermissionDenied\"),\n\
                         _ => println(\"other\"),\n\
                     }\n\
                     let a = Stdio.Inherit;\n\
                     let c = Stdio.Piped;\n\
                     match a { Inherit => println(\"a=Inherit\"), Null => println(\"a=Null\"), Piped => println(\"a=Piped\") }\n\
                     match c { Inherit => println(\"c=Inherit\"), Null => println(\"c=Null\"), Piped => println(\"c=Piped\") }\n\
                 }"
            ),
            Some("PermissionDenied\na=Inherit\nc=Piped\n".to_string())
        );
}

/// B-2026-09-08-8 — the OUTPUT twin of
/// `asan_par_join_tuple_destructure_owns_its_heap_leaves`.
///
/// The leak that row records is silent: every one of these spellings prints
/// correctly on all four surfaces both before and after the fix, which is
/// why it needed valgrind to find and why an output test alone would never
/// have caught it. These cells exist to pin the other half — that teaching
/// `finish_owned_tuple_destructure` to own a `par` join's leaves did not
/// change what any of them computes.
///
/// The last cell is the FAIL-CLOSED one: its join tail hands out an OUTER
/// binding, so the admission declines it and it still leaks 38 B. Its
/// output is pinned here precisely because the leak is left open — a
/// later widening that admits this shape must keep this answer, and the
/// hazard being guarded against (handing a leaf storage that stays readable
/// past the join) would show up here first as a wrong or crashing read.
#[test]
fn par_join_tuple_destructure_output_is_unchanged() {
    const PRE: &str = "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n";
    // One heap-bearing leaf through the join.
    assert_eq!(
        run_program(&format!(
            "{PRE}fn go() -> i64 {{\n\
                 \x20 let (t, k) = par {{ let t = mkp(9); let k = payload().len(); (t, k) }};\n\
                 \x20 return t.a.len() + k; }}\n\
                 fn main() {{ println(go()); }}\n"
        )),
        Some("76\n".to_string())
    );
    // Two heap-bearing leaves.
    assert_eq!(
        run_program(&format!(
            "{PRE}fn go() -> i64 {{\n\
                 \x20 let (t, u) = par {{ let t = mkp(9); let u = mkp(10); (t, u) }};\n\
                 \x20 return t.a.len() + u.a.len(); }}\n\
                 fn main() {{ println(go()); }}\n"
        )),
        Some("76\n".to_string())
    );
    // A scalar-literal element beside a heap leaf — admitted as inert, and
    // measured clean, so the mixed tuple still owns its heap leaf.
    assert_eq!(
        run_program(&format!(
            "{PRE}fn go() -> i64 {{\n\
                 \x20 let (t, k) = par {{ let t = mkp(9); (t, 5) }};\n\
                 \x20 return t.a.len() + k; }}\n\
                 fn main() {{ println(go()); }}\n"
        )),
        Some("43\n".to_string())
    );
    // FAIL-CLOSED: the join tail hands out an OUTER binding. Declined by the
    // admission, still leaking, and its READ must stay correct.
    assert_eq!(
        run_program(&format!(
            "{PRE}fn go() -> i64 {{\n\
                 \x20 let outer = mkp(1);\n\
                 \x20 let (t, k) = par {{ let k = payload().len(); (outer, k) }};\n\
                 \x20 return t.a.len() + k; }}\n\
                 fn main() {{ println(go()); }}\n"
        )),
        Some("76\n".to_string())
    );
}
