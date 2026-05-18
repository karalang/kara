//! Tests for the P1.3 codegen-queries analyzer (phase-7-codegen.md
//! line 25). Each test exercises one detection arm of
//! `codegen_queries::analyze` against a hand-rolled source program and
//! asserts the query kind, ID, and resolution surface.

use karac::codegen_queries::analyze;
use karac::parse;
use karac::queries::{Confidence, Phase, QueryKind};

fn parse_program(src: &str) -> karac::ast::Program {
    let pr = parse(src);
    assert!(
        pr.errors.is_empty(),
        "expected clean parse; got errors: {:?}",
        pr.errors,
    );
    pr.program
}

// ── Inlining-decision queries ──────────────────────────────────────

#[test]
fn inlining_query_fires_for_pub_fn_called_three_times_in_loop() {
    // Three call sites inside a `for` loop body — meets the
    // INLINE_QUERY_LOOP_SITE_THRESHOLD. The function has no
    // `#[inline]`/`#[inline(never)]` so the query is emitted.
    let src = r#"
        pub fn step(x: i64) -> i64 { x + 1 }
        fn main() {
            let mut acc: i64 = 0;
            for i in 0..100 {
                acc = step(acc);
                acc = step(acc);
                acc = step(acc);
            }
            println(f"{acc}");
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    let inlining: Vec<_> = queries
        .iter()
        .filter(|q| q.kind == QueryKind::InliningDecision)
        .collect();
    assert_eq!(
        inlining.len(),
        1,
        "expected one inlining query; got {:?}",
        queries,
    );
    let q = inlining[0];
    assert_eq!(q.id.def_path.render(), "step");
    assert_eq!(q.default_confidence, Confidence::Low);
    assert_eq!(q.cross_phase_origin, Some(Phase::Codegen));
    // Both inlining-resolution attributes are advertised on the
    // resolution surface so external tooling can render either as
    // the fix.
    assert!(q
        .resolution_surface
        .attributes
        .contains(&"inline".to_string()));
    assert!(q
        .resolution_surface
        .attributes
        .contains(&"inline(never)".to_string()));
}

#[test]
fn inlining_query_suppressed_when_pub_fn_already_has_inline_attr() {
    // `#[inline]` resolves the query at the definition — analyzer
    // must skip emission so the report doesn't ask about already-
    // answered decisions.
    let src = r#"
        #[inline]
        pub fn step(x: i64) -> i64 { x + 1 }
        fn main() {
            let mut acc: i64 = 0;
            for i in 0..100 {
                acc = step(acc);
                acc = step(acc);
                acc = step(acc);
            }
            println(f"{acc}");
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::InliningDecision),
        "expected no inlining query when #[inline] is present; got {:?}",
        queries,
    );
}

#[test]
fn inlining_query_suppressed_when_inline_never_present() {
    // The `#[inline(never)]` form resolves the query in the other
    // direction; same suppression.
    let src = r#"
        #[inline(never)]
        pub fn step(x: i64) -> i64 { x + 1 }
        fn main() {
            let mut acc: i64 = 0;
            for i in 0..100 {
                acc = step(acc);
                acc = step(acc);
                acc = step(acc);
            }
            println(f"{acc}");
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::InliningDecision),
        "expected no inlining query when #[inline(never)] is present; got {:?}",
        queries,
    );
}

#[test]
fn inlining_query_does_not_fire_outside_loop() {
    // Calls outside any loop body do not contribute to the loop-site
    // count — they're cold-path-looking from the analyzer's
    // perspective. v1 catalogue convention: the query is about
    // hot-looking sites, not every call.
    let src = r#"
        pub fn step(x: i64) -> i64 { x + 1 }
        fn main() {
            let a = step(0);
            let b = step(a);
            let c = step(b);
            println(f"{c}");
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::InliningDecision),
        "expected no inlining query for cold-only call sites; got {:?}",
        queries,
    );
}

#[test]
fn inlining_query_does_not_fire_for_private_fn() {
    // Only `pub fn` items get a query — internal-only helpers are
    // assumed to be inlined or not based on LLVM's heuristic with
    // no ABI implications.
    let src = r#"
        fn step(x: i64) -> i64 { x + 1 }
        fn main() {
            let mut acc: i64 = 0;
            for i in 0..100 {
                acc = step(acc);
                acc = step(acc);
                acc = step(acc);
            }
            println(f"{acc}");
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::InliningDecision),
        "expected no inlining query for private fn; got {:?}",
        queries,
    );
}

// ── Branch-hint queries ────────────────────────────────────────────

#[test]
fn branch_hint_query_fires_for_asymmetric_match_arms() {
    // Two-arm match with one heavy arm (5 stmts) and one light arm
    // (1 stmt) — 5x ratio trips the BRANCH_HINT_RATIO check.
    let src = r#"
        fn main() {
            let x = 7;
            match x {
                0 => println("zero"),
                _ => {
                    let a = 1;
                    let b = 2;
                    let c = 3;
                    let d = a + b + c;
                    println(f"{d}");
                }
            }
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    let bh: Vec<_> = queries
        .iter()
        .filter(|q| q.kind == QueryKind::BranchHint)
        .collect();
    assert_eq!(
        bh.len(),
        1,
        "expected one branch-hint query; got {:?}",
        queries,
    );
    let q = bh[0];
    assert_eq!(q.id.def_path.render(), "main");
    // Resolution surface lists both `likely` and `unlikely` so the
    // author can mark whichever arm they have a stronger sense for.
    assert!(q
        .resolution_surface
        .attributes
        .contains(&"likely".to_string()));
    assert!(q
        .resolution_surface
        .attributes
        .contains(&"unlikely".to_string()));
    assert_eq!(q.cross_phase_origin, Some(Phase::Codegen));
}

#[test]
fn branch_hint_query_fires_for_asymmetric_if_else() {
    let src = r#"
        fn main() {
            let x = 7;
            if x == 0 {
                println("zero");
            } else {
                let a = 1;
                let b = 2;
                let c = 3;
                let d = a + b + c;
                println(f"{d}");
            }
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    let bh: Vec<_> = queries
        .iter()
        .filter(|q| q.kind == QueryKind::BranchHint)
        .collect();
    assert_eq!(
        bh.len(),
        1,
        "expected one branch-hint query for asymmetric if/else; got {:?}",
        queries,
    );
}

#[test]
fn branch_hint_query_does_not_fire_for_symmetric_match() {
    // Two equal-weight arms — no skew, no query.
    let src = r#"
        fn main() {
            let x = 7;
            match x {
                0 => println("zero"),
                _ => println("nonzero"),
            }
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    assert!(
        !queries.iter().any(|q| q.kind == QueryKind::BranchHint),
        "expected no branch-hint query for symmetric match; got {:?}",
        queries,
    );
}

#[test]
fn branch_hint_query_does_not_fire_for_if_without_else() {
    // No else branch means there's nothing to compare against — the
    // analyzer requires both sides to emit.
    let src = r#"
        fn main() {
            let x = 7;
            if x == 0 {
                let a = 1;
                let b = 2;
                let c = 3;
                let d = a + b + c;
                println(f"{d}");
            }
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    assert!(
        !queries.iter().any(|q| q.kind == QueryKind::BranchHint),
        "expected no branch-hint query for if-without-else; got {:?}",
        queries,
    );
}

#[test]
fn analyze_clean_program_emits_no_queries() {
    // Tiny program with no pub fn, no loops, no asymmetric branches
    // — analyzer is silent. Same shape as `tests/snapshots/clean.kara`
    // (the CLI empty-envelope test).
    let src = r#"
        fn main() {
            let x = 42;
            println(f"{x}");
        }
    "#;
    let program = parse_program(src);
    let queries = analyze(&program);
    assert!(
        queries.is_empty(),
        "expected zero queries for a clean program; got {:?}",
        queries,
    );
}
