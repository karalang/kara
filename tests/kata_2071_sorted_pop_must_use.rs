//! Kata 2071's pool arm takes its workers out of a `SortedSet` with
//! `self.workers.pop_last();` as a statement, and drew a `must_use` warning
//! for the discarded `Option`, while `VecDeque.pop_front` and `Vec.pop`
//! discarded the same way draw none: design.md § `#[must_use]` exempts the
//! container methods whose `Option` reports a displaced value, and its list
//! left out the sorted containers' `pop_first` and `pop_last`. They are now
//! exempt; a user type's `pop_last` still warns.

use karac::ast::Program;
use karac::must_use_lint::{check_implicit_must_use, LintDiagnostic};
use karac::typechecker::TypeCheckResult;

fn parse_and_typecheck(source: &str) -> (Program, TypeCheckResult) {
    let parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    (parsed.program, typed)
}

fn lint(source: &str) -> Vec<LintDiagnostic> {
    let (prog, typed) = parse_and_typecheck(source);
    check_implicit_must_use(
        &prog,
        Some(&typed),
        &karac::lints::CliLintOverrides::default(),
    )
}

#[test]
fn kata_2071_sorted_set_and_map_pops_are_displaced_value_exempt() {
    let diags = lint(
        "fn main() {\n\
             let mut s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(1);\n\
             s.insert(2);\n\
             s.pop_last();\n\
             s.pop_first();\n\
             let mut m: SortedMap[i64, i64] = SortedMap.new();\n\
             m.insert(1, 1);\n\
             m.pop_first();\n\
             m.pop_last();\n\
         }",
    );
    assert!(
        diags.iter().all(|d| d.lint_name != "must_use"),
        "SortedSet / SortedMap pop_first / pop_last must be exempt like VecDeque's pops, got: {diags:?}"
    );
}

#[test]
fn kata_2071_a_user_pop_last_returning_option_still_warns() {
    let diags = lint(
        "struct Pile { n: i64 }\n\
         impl Pile {\n\
             fn pop_last(mut ref self) -> Option[i64] { return None; }\n\
         }\n\
         fn main() {\n\
             let mut p = Pile { n: 0 };\n\
             p.pop_last();\n\
         }",
    );
    assert!(
        diags.iter().any(|d| d.lint_name == "must_use"),
        "a user type's pop_last must still warn, got: {diags:?}"
    );
}
