//! `map_value_clone_reinsert` — B-2026-08-03-9.
//!
//! The lint recognizes the quadratic map-of-containers accumulate idiom and
//! offers `entry(k).or_insert(..)` as a machine-applicable fix. These tests pin
//! both halves of that contract, and — more importantly — the SHAPE BOUNDARY:
//! the lint rewrites working code, so a false positive silently changes program
//! behaviour. Every "must not fire" case below is a rewrite that would be
//! wrong, not merely noisy.

use karac::map_entry_lint::{check_map_value_clone_reinsert, LINT_NAME};

/// Run the front half of the pipeline and return this lint's diagnostics.
fn lint(src: &str) -> Vec<karac::typechecker::TypeError> {
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    check_map_value_clone_reinsert(
        &parsed.program,
        &typed,
        src,
        &karac::lints::CliLintOverrides::default(),
    )
}

/// The canonical accumulate idiom, as the kata corpus wrote it.
const ACCUMULATE: &str = "fn main() {\n\
    let mut index: Map[String, Vec[i64]] = Map.new();\n\
    let w: String = \"the\";\n\
    let i: i64 = 0;\n\
    match index.get(w) {\n\
        Some(existing) => {\n\
            let mut hits: Vec[i64] = existing.clone();\n\
            hits.push(i);\n\
            let _ = index.insert(w, hits);\n\
        }\n\
        None => {\n\
            let mut hits: Vec[i64] = Vec.new();\n\
            hits.push(i);\n\
            let _ = index.insert(w, hits);\n\
        }\n\
    }\n\
}";

#[test]
fn fires_on_the_accumulate_idiom_and_names_the_lint() {
    let diags = lint(ACCUMULATE);
    assert_eq!(diags.len(), 1, "expected exactly one diagnostic: {diags:?}");
    assert_eq!(diags[0].lint_name.as_deref(), Some(LINT_NAME));
    assert!(
        diags[0].message.contains("O(k²)") || diags[0].message.contains("copies"),
        "message should explain the cost; got: {}",
        diags[0].message
    );
}

#[test]
fn fix_it_rewrites_the_match_to_an_entry_call() {
    let diags = lint(ACCUMULATE);
    let fix = diags[0]
        .fix_it
        .as_ref()
        .expect("the diagnostic must carry a machine-applicable fix");
    assert_eq!(
        fix.replacement, "index.entry(w).or_insert(Vec.new()).push(i);",
        "the fix should be the entry-API one-liner"
    );
    // The edit must cover the WHOLE match, or applying it would leave the old
    // arms behind next to the new call.
    let covered = &ACCUMULATE[fix.span.offset..fix.span.offset + fix.span.length];
    assert!(
        covered.starts_with("match index.get(w)") && covered.trim_end().ends_with('}'),
        "fix span should cover the entire match expression; covered: {covered:?}"
    );
}

/// A scalar-valued map's read-modify-write is O(1) — there is no quadratic to
/// remove, so firing here would be pure noise on correct, fast code.
#[test]
fn does_not_fire_on_a_scalar_valued_map() {
    let src = "fn main() {\n\
        let mut counts: Map[String, i64] = Map.new();\n\
        let w: String = \"a\";\n\
        match counts.get(w) {\n\
            Some(c) => {\n\
                let mut n: i64 = c.clone();\n\
                n = n + 1;\n\
                let _ = counts.insert(w, n);\n\
            }\n\
            None => {\n\
                let mut n: i64 = 0;\n\
                n = n + 1;\n\
                let _ = counts.insert(w, n);\n\
            }\n\
        }\n\
    }";
    assert!(lint(src).is_empty(), "scalar-valued map must not fire");
}

/// THE CASE THAT MATTERS MOST. #49's `map_of_lists.kara` has a FOURTH statement
/// in its `None` arm (`order.push(key)`, recording first-seen order). The
/// single-expression rewrite has nowhere to put it, so firing here would
/// silently drop it and change the program's output. Verified against the real
/// kata: the lint stays silent.
#[test]
fn does_not_fire_when_an_arm_does_extra_work() {
    let src = "fn main() {\n\
        let mut table: Map[String, Vec[String]] = Map.new();\n\
        let mut order: Vec[String] = Vec.new();\n\
        let key: String = \"k\";\n\
        let word: String = \"w\";\n\
        match table.get(key) {\n\
            Some(existing) => {\n\
                let mut updated: Vec[String] = existing.clone();\n\
                updated.push(word);\n\
                let _ = table.insert(key, updated);\n\
            }\n\
            None => {\n\
                let mut fresh: Vec[String] = Vec.new();\n\
                fresh.push(word);\n\
                let _ = table.insert(key, fresh);\n\
                order.push(key);\n\
            }\n\
        }\n\
    }";
    assert!(
        lint(src).is_empty(),
        "an arm doing extra work must not be rewritten — the extra statement \
         would be silently dropped"
    );
}

/// Binding `insert`'s result means the DISPLACED value is being used. `or_insert`
/// never displaces anything, so the rewrite would lose that value entirely.
#[test]
fn does_not_fire_when_the_insert_result_is_bound() {
    let src = "fn main() {\n\
        let mut index: Map[String, Vec[i64]] = Map.new();\n\
        let w: String = \"the\";\n\
        let i: i64 = 0;\n\
        match index.get(w) {\n\
            Some(existing) => {\n\
                let mut hits: Vec[i64] = existing.clone();\n\
                hits.push(i);\n\
                let old = index.insert(w, hits);\n\
            }\n\
            None => {\n\
                let mut hits: Vec[i64] = Vec.new();\n\
                hits.push(i);\n\
                let _ = index.insert(w, hits);\n\
            }\n\
        }\n\
    }";
    assert!(
        lint(src).is_empty(),
        "a bound insert result is a different program — the displaced value is used"
    );
}

/// Two arms writing DIFFERENT keys are not one logical append; collapsing them
/// to a single `entry(k)` would send both writes to whichever key was picked.
#[test]
fn does_not_fire_when_the_arms_use_different_keys() {
    let src = "fn main() {\n\
        let mut index: Map[String, Vec[i64]] = Map.new();\n\
        let w: String = \"the\";\n\
        let other: String = \"a\";\n\
        let i: i64 = 0;\n\
        match index.get(w) {\n\
            Some(existing) => {\n\
                let mut hits: Vec[i64] = existing.clone();\n\
                hits.push(i);\n\
                let _ = index.insert(w, hits);\n\
            }\n\
            None => {\n\
                let mut hits: Vec[i64] = Vec.new();\n\
                hits.push(i);\n\
                let _ = index.insert(other, hits);\n\
            }\n\
        }\n\
    }";
    assert!(lint(src).is_empty(), "differing keys must not be merged");
}

/// Without the clone there is no copy to remove — the value was moved out or
/// rebuilt, which is a different program with different ownership.
#[test]
fn does_not_fire_without_the_clone() {
    let src = "fn main() {\n\
        let mut index: Map[String, Vec[i64]] = Map.new();\n\
        let w: String = \"the\";\n\
        let i: i64 = 0;\n\
        match index.get(w) {\n\
            Some(existing) => {\n\
                let mut hits: Vec[i64] = Vec.new();\n\
                hits.push(i);\n\
                let _ = index.insert(w, hits);\n\
            }\n\
            None => {\n\
                let mut hits: Vec[i64] = Vec.new();\n\
                hits.push(i);\n\
                let _ = index.insert(w, hits);\n\
            }\n\
        }\n\
    }";
    assert!(
        lint(src).is_empty(),
        "no clone means nothing quadratic to fix"
    );
}

/// The rewrite is a fixpoint: applying the fix produces code the lint leaves
/// alone. Without this, `karac fix` could oscillate or re-report forever.
#[test]
fn does_not_fire_on_the_fixed_form() {
    let src = "fn main() {\n\
        let mut index: Map[String, Vec[i64]] = Map.new();\n\
        let w: String = \"the\";\n\
        let i: i64 = 0;\n\
        index.entry(w).or_insert(Vec.new()).push(i);\n\
    }";
    assert!(lint(src).is_empty(), "the fixed form must be a fixpoint");
}
