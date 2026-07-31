// tests/index_disjoint.rs
//
// Per-iteration disjointness proof for loops over indexed writes — sub-slice 2
// of "Auto-parallel loops over provably-disjoint indexed writes"
// (`docs/implementation_checklist/phase-6-runtime.md`).
//
// Two properties are pinned here, and the second matters as much as the first:
//
//  1. The shapes the slice scopes IN are proven, with the right stride.
//  2. Every shape it scopes OUT declines, and declines for the *named* reason —
//     because a wrong disjointness claim on indexed writes is a silent
//     miscompile, never a perf regression (`phase-7-codegen.md` § ILP/noalias,
//     citing rustc's `-Zmutable-noalias` saga).
//
// Every case runs through BOTH pipelines: the parser shape and the lowered
// shape (`src/lowering.rs` rewrites primitive binops into
// `Call(Path([i64, "mul"]), …)` before the CLI reaches concurrency analysis).
// A proof that only fires pre-lowering fires only in tests — the exact trap
// `concurrency.rs::induction_step_via_assign` documents for the reduction
// recognizer.

use karac::concurrency::{ConcurrencyAnalysis, DisjointWriteLoop};
use karac::{
    concurrency_analyze, concurrency_analyze_typed, effectcheck, lower, parse, resolve, typecheck,
};

// ── Helpers ─────────────────────────────────────────────────────

fn analyze(source: &str, lowered: bool) -> ConcurrencyAnalysis {
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
    if lowered {
        let resolved = resolve(&parsed.program);
        let tc = typecheck(&parsed.program, &resolved);
        lower(&mut parsed.program, &tc);
    }
    let effects = effectcheck(&parsed.program);
    concurrency_analyze(&parsed.program, &effects)
}

fn loops_of(analysis: &ConcurrencyAnalysis, func: &str) -> Vec<DisjointWriteLoop> {
    analysis
        .function_decisions
        .get(func)
        .map(|fc| fc.disjoint_write_loops.clone())
        .unwrap_or_default()
}

/// Run `body` (a function `k`) through both pipelines and assert the FIRST
/// reported loop's machine tag. Running both is the point — see the file
/// header.
fn assert_tag(source: &str, expected_tag: &str) {
    for lowered in [false, true] {
        let analysis = analyze(source, lowered);
        let loops = loops_of(&analysis, "k");
        assert!(
            !loops.is_empty(),
            "expected a disjoint-write candidate (lowered={lowered}) in:\n{source}"
        );
        assert_eq!(
            loops[0].tag(),
            expected_tag,
            "wrong verdict (lowered={lowered}): {}\nin:\n{source}",
            loops[0].reason,
        );
    }
}

/// Assert the proof discharged and pins the target's stride and base.
fn assert_proven(source: &str, target: &str, stride: &str, base: &str) {
    for lowered in [false, true] {
        let analysis = analyze(source, lowered);
        let loops = loops_of(&analysis, "k");
        assert!(
            !loops.is_empty(),
            "expected a disjoint-write candidate (lowered={lowered}) in:\n{source}"
        );
        let d = &loops[0];
        assert!(
            d.proven(),
            "expected proof to discharge (lowered={lowered}), got {}: {}\nin:\n{source}",
            d.tag(),
            d.reason,
        );
        let t = d
            .targets
            .iter()
            .find(|t| t.target == target)
            .unwrap_or_else(|| panic!("no footprint for `{target}` (lowered={lowered})"));
        assert_eq!(t.stride, stride, "stride (lowered={lowered})");
        assert_eq!(t.base, base, "base (lowered={lowered})");
    }
}

/// The full CLI pipeline, threading the typecheck result into concurrency.
/// Needed by the two soundness gates the caller applies on top of the
/// footprint proof — both consult type information, so an untyped run silently
/// skips them.
fn analyze_with_types(source: &str) -> ConcurrencyAnalysis {
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
    concurrency_analyze_typed(&parsed.program, &effects, Some(&tc))
}

/// Typed-pipeline variant of [`assert_tag`], for the caller-applied gates.
fn assert_tag_typed(source: &str, expected_tag: &str) {
    let analysis = analyze_with_types(source);
    let loops = loops_of(&analysis, "k");
    assert!(
        !loops.is_empty(),
        "expected a disjoint-write candidate in:\n{source}"
    );
    assert_eq!(
        loops[0].tag(),
        expected_tag,
        "wrong verdict: {}\nin:\n{source}",
        loops[0].reason,
    );
}

fn program(body: &str) -> String {
    format!("{body}\nfn main() {{}}\n")
}

// ── Proven: the shapes the slice scopes in ──────────────────────

#[test]
fn test_prism_image_kernel_proves_disjoint() {
    // The motivating workload, written the natural way — no band count, no
    // ceil-div, no `Vec[TaskHandle[Vec[u8]]]`, no manual concat. Iteration `dy`
    // owns exactly `[dy*dw*4, (dy+1)*dw*4)`, which is what the slice design
    // names as the acceptance shape.
    assert_proven(
        &program(
            r#"fn k(dw: i64, dh: i64, out: mut Slice[i64]) {
                for dy in 0..dh {
                    let mut dx: i64 = 0;
                    while dx < dw {
                        for c in 0..4 { out[(dy * dw + dx) * 4 + c] = dy + dx + c; }
                        dx = dx + 1;
                    }
                }
            }"#,
        ),
        "out",
        "4 * dw",
        "0",
    );
}

#[test]
fn test_game_of_life_row_step_proves_disjoint() {
    assert_proven(
        &program(
            r#"fn k(w: i64, h: i64, grid: ref Slice[i64], next: mut Slice[i64]) {
                for y in 0..h { for x in 0..w { next[y * w + x] = grid[y * w + x]; } }
            }"#,
        ),
        "next",
        "w",
        "0",
    );
}

#[test]
fn test_row_wise_matmul_proves_disjoint() {
    assert_proven(
        &program(
            r#"fn k(n: i64, a: ref Slice[i64], out: mut Slice[i64]) {
                for i in 0..n { for j in 0..n { out[i * n + j] = a[i * n + j] * 2; } }
            }"#,
        ),
        "out",
        "n",
        "0",
    );
}

#[test]
fn test_three_deep_nest_folds_symbolic_inner_strides() {
    // `out[(z*H + y)*W + x]`: the coefficient of `y` is the SYMBOLIC `W`, not a
    // constant — so a model that only allowed constant inner strides would
    // decline every volumetric kernel. Residual max is `W*(H-1) + (W-1)` =
    // `H*W - 1`, exactly one below the stride `H*W`.
    assert_proven(
        &program(
            r#"fn k(dd: i64, hh: i64, ww: i64, out: mut Slice[i64]) {
                for z in 0..dd {
                    for y in 0..hh { for x in 0..ww { out[(z * hh + y) * ww + x] = z; } }
                }
            }"#,
        ),
        "out",
        "hh * ww",
        "0",
    );
}

#[test]
fn test_unit_stride_map_over_len_bound_proves_disjoint() {
    // `v.len()` is a genuine loop invariant AND always `>= 0`, which is what
    // lets a footprint whose stride comes from a length discharge.
    assert_proven(
        &program(
            r#"fn k(src: ref Vec[i64], out: mut Slice[i64]) {
                for i in 0..src.len() { out[i] = src[i] * 2; }
            }"#,
        ),
        "out",
        "1",
        "0",
    );
}

#[test]
fn test_invariant_base_offset_shifts_the_tiling_not_breaks_it() {
    // A loop filling a sub-region: `off + i*w + x`. The base shifts every
    // iteration's range by the same amount, so it cannot create an overlap —
    // folding it into the residual instead would reject this wrongly.
    assert_proven(
        &program(
            r#"fn k(n: i64, w: i64, off: i64, out: mut Slice[i64]) {
                for i in 0..n { for x in 0..w { out[off + i * w + x] = x; } }
            }"#,
        ),
        "out",
        "w",
        "off",
    );
}

#[test]
fn test_two_targets_each_get_their_own_footprint() {
    let analysis = analyze(
        &program(
            r#"fn k(n: i64, w: i64, a: mut Slice[i64], b: mut Slice[i64]) {
                for i in 0..n {
                    for x in 0..w { a[i * w + x] = x; }
                    b[i] = i;
                }
            }"#,
        ),
        false,
    );
    let loops = loops_of(&analysis, "k");
    assert_eq!(loops.len(), 1);
    assert!(loops[0].proven(), "{}", loops[0].reason);
    let strides: Vec<(String, String)> = loops[0]
        .targets
        .iter()
        .map(|t| (t.target.clone(), t.stride.clone()))
        .collect();
    assert_eq!(
        strides,
        vec![
            ("a".to_string(), "w".to_string()),
            ("b".to_string(), "1".to_string()),
        ]
    );
}

#[test]
fn test_conditional_write_stays_inside_the_footprint() {
    // Writing on only some iterations narrows the footprint; it cannot widen it.
    assert_proven(
        &program(
            r#"fn k(n: i64, w: i64, out: mut Slice[i64]) {
                for i in 0..n {
                    for x in 0..w { if x > 2 { out[i * w + x] = x; } }
                }
            }"#,
        ),
        "out",
        "w",
        "0",
    );
}

#[test]
fn test_let_bound_row_base_is_substituted() {
    // `let row = i * w;` then `out[row + x]` — the index names a body-local, so
    // the proof must substitute its form rather than give up on the name.
    assert_proven(
        &program(
            r#"fn k(n: i64, w: i64, out: mut Slice[i64]) {
                for i in 0..n {
                    let row: i64 = i * w;
                    for x in 0..w { out[row + x] = x; }
                }
            }"#,
        ),
        "out",
        "w",
        "0",
    );
}

#[test]
fn test_inclusive_inner_range_widens_the_residual_bound() {
    // `0..=w` reaches `w`, so the residual max is `w`, not `w-1`, and a stride
    // of `w` no longer covers it. Getting `..=` wrong is an off-by-one that
    // produces an overlap of exactly one slot per iteration.
    assert_tag(
        &program(
            r#"fn k(n: i64, w: i64, out: mut Slice[i64]) {
                for i in 0..n { for x in 0..=w { out[i * w + x] = x; } }
            }"#,
        ),
        "footprint_overlap",
    );
    // With the stride widened to match, the same loop proves.
    assert_proven(
        &program(
            r#"fn k(n: i64, w: i64, out: mut Slice[i64]) {
                for i in 0..n { for x in 0..=w { out[i * (w + 1) + x] = x; } }
            }"#,
        ),
        "out",
        "1 + w",
        "0",
    );
}

// ── Declined: the shapes the slice scopes out ───────────────────

#[test]
fn test_indirect_index_declines() {
    // `out[idx[i]]` — explicitly out of scope; no static proof can bound it.
    assert_tag(
        &program(
            r#"fn k(n: i64, idx: ref Slice[i64], out: mut Slice[i64]) {
                for i in 0..n { out[idx[i]] = i; }
            }"#,
        ),
        "indirect_index",
    );
}

#[test]
fn test_inner_range_wider_than_the_stride_declines() {
    // `out[i*10 + j]` with `j in 0..w`: fine when `w <= 10`, an overlap
    // otherwise. `w` is unknown, so the honest answer is decline.
    assert_tag(
        &program(
            r#"fn k(n: i64, w: i64, out: mut Slice[i64]) {
                for i in 0..n { for j in 0..w { out[i * 10 + j] = j; } }
            }"#,
        ),
        "footprint_overlap",
    );
}

#[test]
fn test_bare_row_base_declines_because_the_stride_may_be_zero() {
    // `out[i * dw]` with NO inner loop over `dw`. If `dw == 0` every iteration
    // writes slot 0 — a real collision. Nothing in scope forces `dw >= 1`, so
    // this must decline. This is the case an "atoms are positive" shortcut
    // would silently get wrong.
    assert_tag(
        &program(
            r#"fn k(n: i64, dw: i64, out: mut Slice[i64]) {
                for i in 0..n { out[i * dw] = i; }
            }"#,
        ),
        "footprint_overlap",
    );
}

#[test]
fn test_same_row_base_proves_once_an_inner_loop_forces_a_positive_stride() {
    // The contrast case for the test above: reaching a write inside
    // `for x in 0..dw` means the loop executed, hence `dw >= 1`, hence the
    // ranges are non-degenerate.
    assert_proven(
        &program(
            r#"fn k(n: i64, dw: i64, out: mut Slice[i64]) {
                for i in 0..n { for x in 0..dw { out[i * dw + x] = x; } }
            }"#,
        ),
        "out",
        "dw",
        "0",
    );
}

#[test]
fn test_a_sibling_loops_bound_does_not_leak_its_sign() {
    // `for x in 0..w` says `w >= 1` only INSIDE its own body — an unexecuted
    // loop says nothing at all. Here the write sits in a SIBLING loop, and its
    // slack polynomial is exactly `w`: accepting it would mean trusting a fact
    // scoped to a loop the write is not inside, and a negative `w` shrinks the
    // stride below the residual's range.
    assert_tag(
        &program(
            r#"fn k(n: i64, w: i64, m: i64, out: mut Slice[i64]) {
                for i in 0..n {
                    for x in 0..w { let t: i64 = x; }
                    for y in 0..m { out[i * (m + w) + y] = y; }
                }
            }"#,
        ),
        "footprint_overlap",
    );
}

#[test]
fn test_index_without_the_loop_variable_declines() {
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                for i in 0..n { out[3] = i; }
            }"#,
        ),
        "invariant_write_slot",
    );
}

#[test]
fn test_reading_a_written_target_declines() {
    // `out[i] = out[i-1] + 1` is a genuine loop-carried dependency.
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                for i in 1..n { out[i] = out[i - 1] + 1; }
            }"#,
        ),
        "reads_written_target",
    );
}

#[test]
fn test_scalar_write_to_an_outer_binding_declines() {
    // The accumulator is a reduction — a different fan-out mechanism with its
    // own combine step. This proof does not model it, so it must not claim the
    // loop's writes are disjoint.
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                let mut total: i64 = 0;
                for i in 0..n { out[i] = i; total = total + i; }
            }"#,
        ),
        "other_outer_write",
    );
}

#[test]
fn test_quadratic_index_declines() {
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                for i in 0..n { out[i * i] = i; }
            }"#,
        ),
        "non_affine_index",
    );
}

#[test]
fn test_division_in_the_index_declines() {
    // `i / 2` maps two iterations onto one slot.
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                for i in 0..n { out[i / 2] = i; }
            }"#,
        ),
        "non_affine_index",
    );
}

#[test]
fn test_two_writes_with_different_strides_decline() {
    // Iteration 0's `out[1]` (from the `i*8+1` write) and iteration 0's
    // `out[0]` tile the buffer two different ways; their ranges cross at some
    // iteration pair.
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                for i in 0..n { out[i * 4] = i; out[i * 8 + 1] = i; }
            }"#,
        ),
        "stride_mismatch",
    );
}

#[test]
fn test_early_return_declines() {
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) -> i64 {
                for i in 0..n { if i > 3 { return i; } out[i] = i; }
                0
            }"#,
        ),
        "early_exit",
    );
}

#[test]
fn test_break_out_of_the_candidate_loop_declines() {
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                for i in 0..n { if i > 3 { break; } out[i] = i; }
            }"#,
        ),
        "early_exit",
    );
}

#[test]
fn test_closure_in_the_body_declines() {
    // A closure can capture and mutate anything; the walk does not model it.
    assert_tag(
        &program(
            r#"fn k(n: i64, out: mut Slice[i64]) {
                for i in 0..n {
                    let f = |x: i64| x + 1;
                    out[i] = f(i);
                }
            }"#,
        ),
        "opaque_body_construct",
    );
}

#[test]
fn test_counter_advanced_twice_makes_the_inner_bound_unusable() {
    // The self-hosted lexer's `if escaped { i = i + 1 }` skip-advance shape: a
    // mid-body extra increment breaks `dx < dw` for every statement after it,
    // so `dx` is no longer bounded by `[0, dw)` at the write.
    assert_tag(
        &program(
            r#"fn k(n: i64, dw: i64, out: mut Slice[i64]) {
                for i in 0..n {
                    let mut dx: i64 = 0;
                    while dx < dw {
                        if dx > 2 { dx = dx + 1; }
                        out[i * dw + dx] = dx;
                        dx = dx + 1;
                    }
                }
            }"#,
        ),
        "unbounded_inner_loop",
    );
}

#[test]
fn test_unbounded_inner_loop_variable_declines() {
    // A `loop { }` counter has no invariant `[lo, hi)` bound at all.
    assert_tag(
        &program(
            r#"fn k(n: i64, dw: i64, out: mut Slice[i64]) {
                for i in 0..n {
                    let mut dx: i64 = 0;
                    loop {
                        out[i * dw + dx] = dx;
                        dx = dx + 1;
                        if dx > dw { break; }
                    }
                }
            }"#,
        ),
        "unbounded_inner_loop",
    );
}

// ── Candidate selection ─────────────────────────────────────────

#[test]
fn test_loop_without_an_indexed_write_is_not_a_candidate() {
    // A scalar reduction is `loop_reductions`' business. Reporting it here as a
    // declined disjoint-write loop would bury the real declines in noise.
    for lowered in [false, true] {
        let analysis = analyze(
            &program(
                r#"fn k(n: i64) -> i64 {
                    let mut s: i64 = 0;
                    for i in 0..n { s = s + i; }
                    s
                }"#,
            ),
            lowered,
        );
        assert!(
            loops_of(&analysis, "k").is_empty(),
            "a pure reduction loop must not appear as a disjoint-write candidate (lowered={lowered})"
        );
    }
}

#[test]
fn test_a_proven_outer_loop_suppresses_its_inner_corollaries() {
    // Every inner loop of a disjoint nest is trivially disjoint too. The scope
    // is "parallelize the OUTER loop", so reporting all of them would bury the
    // decision that matters under its own consequences.
    let analysis = analyze(
        &program(
            r#"fn k(w: i64, h: i64, out: mut Slice[i64]) {
                for y in 0..h { for x in 0..w { out[y * w + x] = x; } }
            }"#,
        ),
        false,
    );
    let loops = loops_of(&analysis, "k");
    assert_eq!(loops.len(), 1, "got {loops:#?}");
    assert_eq!(loops[0].loop_var, "y");
}

#[test]
fn test_a_declined_outer_loop_still_surfaces_the_inner_candidate() {
    // The outer loop declines (`out[i*10 + j]` can overlap), but the inner one
    // is a real per-`j` footprint — a developer asking "why isn't my loop
    // parallel" should see both answers.
    let analysis = analyze(
        &program(
            r#"fn k(n: i64, w: i64, out: mut Slice[i64]) {
                for i in 0..n { for j in 0..w { out[i * 10 + j] = j; } }
            }"#,
        ),
        false,
    );
    let loops = loops_of(&analysis, "k");
    assert_eq!(loops.len(), 2, "got {loops:#?}");
    assert_eq!(loops[0].loop_var, "i");
    assert!(!loops[0].proven());
    assert_eq!(loops[1].loop_var, "j");
    assert!(loops[1].proven());
}

// ── Soundness gates applied by the caller ───────────────────────

#[test]
fn test_shared_value_in_the_body_declines_non_atomic_refcount() {
    // B-2026-07-16-6. Disjoint ELEMENT writes do not make the refcount traffic
    // disjoint: a plain (non-`par`) `shared` header is non-atomic, and a racing
    // rc-inc/rc-dec pair frees a live object.
    assert_tag_typed(
        &program(
            r#"shared struct Node { v: i64 }
            fn k(n: i64, nodes: ref Vec[Node], out: mut Slice[i64]) {
                for i in 0..n {
                    let cur: Node = nodes[i];
                    out[i] = cur.v + i;
                }
            }"#,
        ),
        "not_cross_task_safe",
    );
}

#[test]
fn test_mut_ref_argument_to_a_callee_declines() {
    // B-2026-07-23-20. `scratch` is allocated once outside the loop and
    // mutated INSIDE the callee, where the footprint walk cannot see it — every
    // iteration writes the same buffer.
    assert_tag_typed(
        &program(
            r#"fn helper(s: mut ref Vec[i64], v: i64) -> i64 { s.push(v); v }
            fn k(n: i64, out: mut Slice[i64]) {
                let mut scratch: Vec[i64] = Vec.new();
                for i in 0..n { out[i] = helper(mut scratch, i); }
            }"#,
        ),
        "shares_outer_mut_borrow",
    );
}

// ── Query surface ───────────────────────────────────────────────

#[test]
fn test_query_reason_names_the_proven_interval() {
    // The slice design specifies this prose shape as the acceptance surface:
    // a declined loop reports *why*, and a proven one names the interval.
    let analysis = analyze(
        &program(
            r#"fn k(dw: i64, dh: i64, out: mut Slice[i64]) {
                for dy in 0..dh {
                    for dx in 0..dw { for c in 0..4 { out[(dy * dw + dx) * 4 + c] = c; } }
                }
            }"#,
        ),
        false,
    );
    let loops = loops_of(&analysis, "k");
    assert_eq!(
        loops[0].reason,
        "iteration `dy` writes `out` only within [dy * (4 * dw), (dy + 1) * (4 * dw))"
    );
    assert_eq!(loops[0].tag(), "proven");
}

#[test]
fn test_every_decline_carries_a_distinct_tag_and_a_nonempty_reason() {
    // The whole point of the surface: "the compiler silently didn't
    // parallelize" is the failure mode a queryable decline replaces, so an
    // empty or duplicated tag would defeat it.
    let cases: Vec<(&str, &str)> = vec![
        (
            "indirect_index",
            r#"fn k(n: i64, idx: ref Slice[i64], out: mut Slice[i64]) {
                for i in 0..n { out[idx[i]] = i; }
            }"#,
        ),
        (
            "non_affine_index",
            r#"fn k(n: i64, out: mut Slice[i64]) { for i in 0..n { out[i * i] = i; } }"#,
        ),
        (
            "invariant_write_slot",
            r#"fn k(n: i64, out: mut Slice[i64]) { for i in 0..n { out[7] = i; } }"#,
        ),
        (
            "reads_written_target",
            r#"fn k(n: i64, out: mut Slice[i64]) { for i in 1..n { out[i] = out[i - 1]; } }"#,
        ),
        (
            "early_exit",
            r#"fn k(n: i64, out: mut Slice[i64]) -> i64 {
                for i in 0..n { if i > 1 { return i; } out[i] = i; }
                0
            }"#,
        ),
    ];
    let mut seen: Vec<String> = Vec::new();
    for (tag, body) in cases {
        let analysis = analyze(&program(body), false);
        let loops = loops_of(&analysis, "k");
        assert_eq!(loops[0].tag(), tag);
        assert!(
            !loops[0].reason.is_empty(),
            "decline `{tag}` must carry prose"
        );
        assert!(!seen.contains(&tag.to_string()), "duplicate tag {tag}");
        seen.push(tag.to_string());
    }
}
